use actix_web::middleware::from_fn;
use actix_web::{web, App, HttpServer};
use sqlx::postgres::PgPoolOptions;

use ods_doceditor::api::extractors::JwtConfig;
use ods_doceditor::api::middleware::correlate;
use ods_doceditor::api::{documents, health, versions};
use ods_doceditor::config::AppConfig;
use ods_doceditor::events::producer::producer_from_config;
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

    // PostgreSQL connection pool
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute("SET search_path = editor, public;")
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
    {
        use sqlx::Executor;
        pool.execute("CREATE SCHEMA IF NOT EXISTS editor")
            .await
            .expect("Failed to ensure the editor schema exists");
    }

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run database migrations");
    tracing::info!("Database migrations applied");

    // Say out loud whether the database will actually enforce the policies.
    ods_doceditor::repository::tenant_context::log_rls_posture(&pool).await;

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
