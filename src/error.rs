use actix_web::{HttpResponse, ResponseError};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl ResponseError for AppError {
    fn error_response(&self) -> HttpResponse {
        match self {
            AppError::Validation(msg) => HttpResponse::UnprocessableEntity()
                .json(serde_json::json!({"error": "validation_error", "message": msg})),
            AppError::NotFound(msg) => HttpResponse::NotFound()
                .json(serde_json::json!({"error": "not_found", "message": msg})),
            AppError::Unauthorized(msg) => HttpResponse::Unauthorized()
                .json(serde_json::json!({"error": "unauthorized", "message": msg})),
            AppError::BadRequest(msg) => HttpResponse::BadRequest()
                .json(serde_json::json!({"error": "bad_request", "message": msg})),
            AppError::Internal(msg) => {
                tracing::error!("Internal error: {}", msg);
                HttpResponse::InternalServerError().json(
                    serde_json::json!({"error": "internal_error", "message": "An internal error occurred"}),
                )
            }
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::NotFound("Resource not found".to_string()),
            _ => AppError::Internal(format!("Database error: {e}")),
        }
    }
}
