use actix_web::{web, HttpResponse};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::extractors::AuthUser;
use crate::error::AppError;
use crate::service::document_service::DocumentService;

#[derive(Debug, Deserialize)]
pub struct CreateVersionRequest {
    pub comment: Option<String>,
}

/// POST /api/v1/documents/{id}/versions
pub async fn create_version(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    path: web::Path<Uuid>,
    body: web::Json<CreateVersionRequest>,
) -> Result<HttpResponse, AppError> {
    let document_id = path.into_inner();

    // BR-022: validate comment length
    if let Some(ref comment) = body.comment {
        if comment.len() > 500 {
            return Err(AppError::Validation(
                "Comment must be at most 500 characters".to_string(),
            ));
        }
    }

    let version = svc
        .create_version(
            auth.tenant_id,
            document_id,
            auth.user_id,
            body.comment.as_deref(),
        )
        .await?;

    Ok(HttpResponse::Created().json(serde_json::json!({
        "version": version.version,
        "created_at": version.created_at,
        "created_by": version.created_by,
        "comment": version.comment,
    })))
}

/// GET /api/v1/documents/{id}/versions
pub async fn list_versions(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, AppError> {
    let document_id = path.into_inner();
    let versions = svc.list_versions(auth.tenant_id, document_id).await?;

    let response: Vec<serde_json::Value> = versions
        .iter()
        .map(|v| {
            serde_json::json!({
                "version": v.version,
                "created_at": v.created_at,
                "created_by": v.created_by,
                "snapshot_size_bytes": v.snapshot_size_bytes,
                "comment": v.comment,
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "versions": response,
    })))
}

/// GET /api/v1/documents/{doc_id}/versions/{version}
pub async fn get_version(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    path: web::Path<(Uuid, i32)>,
) -> Result<HttpResponse, AppError> {
    let (document_id, version_number) = path.into_inner();
    let version = svc
        .get_version(auth.tenant_id, document_id, version_number)
        .await?;

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "version": version.version,
        "created_at": version.created_at,
        "created_by": version.created_by,
        "snapshot_size_bytes": version.snapshot_size_bytes,
        "comment": version.comment,
        "is_auto": version.is_auto,
    })))
}
