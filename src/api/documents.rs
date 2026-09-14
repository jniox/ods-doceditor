use actix_web::{web, HttpResponse};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::extractors::AuthUser;
use crate::domain::document::DocumentUpdate;
use crate::domain::pagination::Pagination;
use crate::domain::text::Title;
use crate::error::AppError;
use crate::service::document_service::DocumentService;

#[derive(Debug, Deserialize)]
pub struct CreateDocumentRequest {
    pub title: String,
    /// The document body. Mutually exclusive with `template_id`.
    pub content: Option<String>,
    /// Start from a template's body instead of supplying one.
    pub template_id: Option<Uuid>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDocumentRequest {
    pub title: Option<String>,
    pub status: Option<String>,
    pub metadata: Option<serde_json::Value>,
    /// A new body. Supplying it advances the version and writes an immutable
    /// snapshot of the previous state's successor.
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListDocumentsQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub status: Option<String>,
    pub search: Option<String>,
}

/// POST /api/v1/documents
pub async fn create_document(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    body: web::Json<CreateDocumentRequest>,
) -> Result<HttpResponse, AppError> {
    let metadata = body.metadata.clone().unwrap_or(serde_json::json!({}));

    let doc = svc
        .create_document(
            auth.tenant_id,
            &body.title,
            auth.user_id,
            metadata,
            body.content.as_deref(),
            body.template_id,
        )
        .await?;

    Ok(HttpResponse::Created().json(doc))
}

/// GET /api/v1/documents
///
/// The response reports the page that was **served**, not the one that was
/// **asked for**. Those differ exactly when the server normalised something —
/// `page=0` is served as page 1, `per_page=1000` as 100 — and that is precisely
/// when the caller needs to be told: a client that paginates on the numbers it
/// sent back computes `ceil(total / 1000)` pages and stops after the first, or
/// walks `0, 1, 2` and reads the first page twice. Building the `Pagination`
/// here and rendering the response from that same value leaves no second number
/// in scope to report by mistake.
pub async fn list_documents(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    query: web::Query<ListDocumentsQuery>,
) -> Result<HttpResponse, AppError> {
    let pagination = Pagination::new(query.page, query.per_page);

    let (docs, total) = svc
        .list_documents(
            auth.tenant_id,
            pagination,
            query.status.as_deref(),
            query.search.as_deref(),
        )
        .await?;

    Ok(HttpResponse::Ok().json(serde_json::json!({
        "documents": docs,
        "total": total,
        "page": pagination.page(),
        "per_page": pagination.per_page(),
    })))
}

/// GET /api/v1/documents/{id}
pub async fn get_document(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, AppError> {
    let document_id = path.into_inner();
    let doc = svc.get_document(auth.tenant_id, document_id).await?;
    Ok(HttpResponse::Ok().json(doc))
}

/// PATCH /api/v1/documents/{id}
///
/// The title is parsed here, at the boundary, exactly as the page is: what
/// travels on is the trimmed, bounded value, so no layer downstream can check
/// one string and store another. That split is what made a rename to a
/// 500-character title with a leading space answer `500`.
pub async fn update_document(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    path: web::Path<Uuid>,
    body: web::Json<UpdateDocumentRequest>,
) -> Result<HttpResponse, AppError> {
    let document_id = path.into_inner();
    let title = body.title.as_deref().map(Title::parse).transpose()?;

    let doc = svc
        .update_document(
            auth.tenant_id,
            document_id,
            auth.user_id,
            DocumentUpdate {
                title,
                status: body.status.as_deref(),
                metadata: body.metadata.clone(),
                content: body.content.as_deref(),
            },
        )
        .await?;

    Ok(HttpResponse::Ok().json(doc))
}

/// DELETE /api/v1/documents/{id}
pub async fn delete_document(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, AppError> {
    let document_id = path.into_inner();
    svc.delete_document(auth.tenant_id, document_id, auth.user_id)
        .await?;
    Ok(HttpResponse::NoContent().finish())
}
