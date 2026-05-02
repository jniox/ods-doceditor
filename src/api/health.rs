use actix_web::{HttpResponse, web};
use sqlx::PgPool;

/// GET /health — liveness probe (AC-020).
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "ok",
        "service": "doceditor",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// GET /ready — readiness probe (checks DB connection).
pub async fn ready(pool: web::Data<PgPool>) -> HttpResponse {
    match sqlx::query("SELECT 1").execute(pool.get_ref()).await {
        Ok(_) => HttpResponse::Ok().json(serde_json::json!({
            "status": "ok",
            "database": "connected",
        })),
        Err(_) => HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "status": "unavailable",
            "database": "disconnected",
        })),
    }
}
