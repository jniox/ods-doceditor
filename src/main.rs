use std::sync::Arc;

use actix_web::{App, HttpServer, web};
use sqlx::postgres::PgPoolOptions;

use ods_doceditor::api::{documents, health, versions};
use ods_doceditor::config::AppConfig;
use ods_doceditor::events::producer::NoopProducer;
use ods_doceditor::service::document_service::DocumentService;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load .env (dev convenience)
    let _ = dotenvy::dotenv();

    // Tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Config
    let config = AppConfig::from_env();

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

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run database migrations");
    tracing::info!("Database migrations applied");

    // Event producer (NoopProducer until Redpanda is configured)
    let producer: Arc<dyn ods_doceditor::events::producer::EventProducer> =
        Arc::new(NoopProducer);

    // Document service
    let doc_service = DocumentService::new(pool.clone(), producer);

    let bind = format!("{}:{}", config.server_host, config.server_port);
    tracing::info!("Starting DocEditor on {}", bind);

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(doc_service.clone()))
            // Health endpoints (no auth)
            .route("/health", web::get().to(health::health))
            .route("/ready", web::get().to(health::ready))
            // Document CRUD
            .service(
                web::scope("/api/v1")
                    .route("/documents", web::post().to(documents::create_document))
                    .route("/documents", web::get().to(documents::list_documents))
                    .route("/documents/{id}", web::get().to(documents::get_document))
                    .route("/documents/{id}", web::patch().to(documents::update_document))
                    .route("/documents/{id}", web::delete().to(documents::delete_document))
                    // Versions
                    .route("/documents/{id}/versions", web::post().to(versions::create_version))
                    .route("/documents/{id}/versions", web::get().to(versions::list_versions))
                    .route("/documents/{doc_id}/versions/{version}", web::get().to(versions::get_version)),
            )
    })
    .bind(&bind)?
    .run()
    .await
}
