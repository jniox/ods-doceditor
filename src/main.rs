use actix_web::middleware::from_fn;
use actix_web::{web, App, HttpServer};
use sqlx::postgres::PgPoolOptions;

use ods_doceditor::api::extractors::JwtConfig;
use ods_doceditor::api::middleware::correlate;
use ods_doceditor::api::{documents, health, versions};
use ods_doceditor::config::AppConfig;
use ods_doceditor::events::producer::producer_from_config;
use ods_doceditor::repository::tenant_context;
use ods_doceditor::service::document_service::DocumentService;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load .env (dev convenience)
    let _ = dotenvy::dotenv();

    // Config first: the log filter is part of it.
    let config = AppConfig::from_env();

    // Tracing. Structured JSON on one line per event, so Cloud Logging parses
    // the fields (correlation id included) instead of a wall of text.
    let filter = config.log_filter();
    let env_filter = tracing_subscriber::EnvFilter::try_new(&filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_env_filter(env_filter)
        .init();
    tracing::info!(filter = %filter, "Logging configured");

    // JWT configuration
    let jwt_config = build_jwt_config(&config);
    tracing::info!("JWT authentication configured");

    // ── PostgreSQL: the boot pool ──────────────────────────────────────────
    // Deliberately separate from the pool that serves requests, and closed as
    // soon as the schema is in place. Creating a schema, running migrations and
    // provisioning the runtime role are administrative acts; serving a request
    // is not, and since migration 008 the two no longer run as the same role.
    let boot_pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute(tenant_context::session_setup(false).as_str())
                    .await
                    .map(|_| ())
            })
        })
        .connect(&config.database_url)
        .await
        .expect("Failed to connect to PostgreSQL");

    tracing::info!("Connected to PostgreSQL (search_path = editor, public)");

    // Must precede the migrations: sqlx creates `_sqlx_migrations` with an
    // unqualified CREATE TABLE *before* running migration 001, which is the
    // migration that creates this schema. PostgreSQL silently drops a
    // non-existent schema from `search_path`, so on a fresh database the
    // tracking table would land in `public` — shared with other services.
    // Concurrency-safe: `IF NOT EXISTS` is a look followed by an insert and
    // races against itself, so two instances starting together on a fresh
    // database would crash-loop one of them. See repository::schema.
    ods_doceditor::repository::schema::ensure_schema_exists(&boot_pool, "editor")
        .await
        .expect("Failed to ensure the editor schema exists");

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&boot_pool)
        .await
        .expect("Failed to run database migrations");
    tracing::info!("Database migrations applied");

    // ── The serving pool drops its own privileges ──────────────────────────
    // `DATABASE_URL` resolves to a privileged role in every environment this
    // service has ever run in — `ods`, `rolsuper = t, rolbypassrls = t` — and
    // PostgreSQL exempts such a role from every policy, whatever migration 007
    // marks FORCE. Seven BA cycles filed that as "operational, not code" and
    // nothing moved, because a repository cannot rotate a secret.
    //
    // It does not need to. Policies are evaluated against the EFFECTIVE role, so
    // a session that runs `SET ROLE editor_app` (migration 008) is subject to all
    // of them from that point on. That is what every connection of this pool does
    // — and why the posture measured below now reads `editor_app` rather than the
    // superuser that opened the socket. See tests/rls_enforcement_test.rs.
    let adopt_runtime_role = tenant_context::runtime_role_is_adoptable(&boot_pool)
        .await
        .expect("Could not check the runtime role");

    if adopt_runtime_role {
        tracing::info!(
            role = tenant_context::RUNTIME_ROLE,
            "Serving pool drops into the runtime role on every connection"
        );
    } else {
        tracing::warn!(
            role = tenant_context::RUNTIME_ROLE,
            remediation = tenant_context::REMEDIATION,
            "The runtime role is unavailable — every query will run with the privileges of \
             the connection string, which bypasses row-level security when they include \
             SUPERUSER or BYPASSRLS"
        );
    }

    let session_setup = tenant_context::session_setup(adopt_runtime_role);
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .after_connect(move |conn, _meta| {
            let setup = session_setup.clone();
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute(setup.as_str()).await.map(|_| ())
            })
        })
        .connect(&config.database_url)
        .await
        .expect("Failed to open the serving connection pool");
    boot_pool.close().await;

    // Say out loud whether the database will actually enforce the policies, for
    // the role the requests will really run as.
    tenant_context::log_rls_posture(&pool).await;

    // Event producer. A real one whenever REDPANDA_BROKERS says where to publish.
    let producer = producer_from_config(config.redpanda_brokers.as_deref(), &config.redpanda_topic);
    tracing::info!(producer = producer.name(), "Event producer wired");

    // Payload limits based on max_document_size_mb
    let max_payload_bytes = config.max_document_size_mb * 1024 * 1024;

    // Document service
    let doc_service =
        DocumentService::new(pool.clone(), producer).with_max_content_bytes(max_payload_bytes);

    let bind = format!("{}:{}", config.server_host, config.server_port);
    tracing::info!("Starting DocEditor on {}", bind);

    HttpServer::new(move || {
        App::new()
            // Outermost: every request, health probes included, gets a
            // correlation id and echoes it back.
            .wrap(from_fn(correlate))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(doc_service.clone()))
            .app_data(web::Data::new(jwt_config.clone()))
            // Wire max document size to JSON and payload config
            .app_data(web::JsonConfig::default().limit(max_payload_bytes))
            .app_data(web::PayloadConfig::default().limit(max_payload_bytes))
            // Health endpoints (no auth)
            .route("/health", web::get().to(health::health))
            // BR-0016: the image must answer 200 on BOTH paths. `/healthz` is
            // measurable on the CONTAINER only — once deployed, Google's front
            // end intercepts it above Cloud Run and returns its own 404 before
            // the container is ever reached. A deployed probe therefore uses
            // `/health`, which is what ops/cloudrun/doceditor.json declares.
            .route("/healthz", web::get().to(health::health))
            .route("/ready", web::get().to(health::ready))
            // Document CRUD
            .service(
                web::scope("/api/v1")
                    .route("/documents", web::post().to(documents::create_document))
                    .route("/documents", web::get().to(documents::list_documents))
                    .route("/documents/{id}", web::get().to(documents::get_document))
                    .route(
                        "/documents/{id}",
                        web::patch().to(documents::update_document),
                    )
                    .route(
                        "/documents/{id}",
                        web::delete().to(documents::delete_document),
                    )
                    // Versions
                    .route(
                        "/documents/{id}/versions",
                        web::post().to(versions::create_version),
                    )
                    .route(
                        "/documents/{id}/versions",
                        web::get().to(versions::list_versions),
                    )
                    .route(
                        "/documents/{doc_id}/versions/{version}",
                        web::get().to(versions::get_version),
                    ),
            )
    })
    .bind(&bind)?
    .run()
    .await
}

fn build_jwt_config(config: &AppConfig) -> JwtConfig {
    let issuer = config.jwt_issuer.as_deref();
    let audience = config.jwt_audience.as_deref();

    // Prefer RS256 with RSA public key
    if let Some(ref pem_b64) = config.jwt_rsa_public_key_b64 {
        return JwtConfig::from_rsa_pem_b64(pem_b64, issuer, audience)
            .expect("Invalid JWT_RSA_PUBLIC_KEY_B64");
    }

    // Fall back to HS256 for dev/test if explicitly allowed
    if config.jwt_allow_hs256 {
        let secret = config
            .jwt_secret
            .as_deref()
            .expect("JWT_SECRET must be set when JWT_ALLOW_HS256=true");
        return JwtConfig::from_hs256_secret(secret, issuer, audience);
    }

    panic!("JWT not configured: set JWT_RSA_PUBLIC_KEY_B64 (production) or JWT_ALLOW_HS256=true + JWT_SECRET (dev)");
}
