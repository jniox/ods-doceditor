use actix_web::{web, HttpResponse};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::extractors::AuthUser;
use crate::domain::document::DocumentUpdate;
use crate::domain::metadata::Metadata;
use crate::domain::pagination::Pagination;
use crate::domain::query::supplied;
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

/// The query string exactly as it arrived: four strings, typed by nobody.
///
/// `page` and `per_page` were `Option<i64>` until 2026-09-14, which reads like
/// a convenience and is a delegation: `serde` then decided what a malformed
/// page was, and answered it from a layer this service does not write —
/// `400 text/plain`, "invalid digit found in string", outside the closed
/// enumeration `docs/openapi.yaml` publishes. It also made `?page=` — a form
/// field nobody typed into — indistinguishable from a typo.
///
/// Strings here, values built below: the same move as `title`, `metadata` and
/// the page itself. See [`crate::domain::query`] and
/// `tests/query_contract_test.rs`.
#[derive(Debug, Deserialize)]
pub struct ListDocumentsQuery {
    pub page: Option<String>,
    pub per_page: Option<String>,
    pub status: Option<String>,
    pub search: Option<String>,
}

/// POST /api/v1/documents
pub async fn create_document(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    body: web::Json<CreateDocumentRequest>,
) -> Result<HttpResponse, AppError> {
    // Parsed at the boundary, exactly like the title below it: what travels on
    // is the bounded value, and the `clone()` this line used to do first now
    // happens inside `parse`, after the size bound has been checked.
    let metadata = body
        .metadata
        .as_ref()
        .map(Metadata::parse)
        .transpose()?
        .unwrap_or_default();

    let doc = svc
        .create_document(
            auth.tenant_id,
            &body.title,
            auth.user_id,
            &metadata,
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
///
/// The four parameters are read through [`supplied`], so *the field was left
/// empty* is one gesture with one answer — the parameter was not supplied —
/// instead of the four it used to have: `400 text/plain` for `page` and
/// `per_page`, `400 application/json` for `status`, and, worst of the four
/// because it is silent, `200` with an empty page for `search`.
pub async fn list_documents(
    auth: AuthUser,
    svc: web::Data<DocumentService>,
    query: web::Query<ListDocumentsQuery>,
) -> Result<HttpResponse, AppError> {
    let pagination = Pagination::parse(query.page.as_deref(), query.per_page.as_deref())?;

    let (docs, total) = svc
        .list_documents(
            auth.tenant_id,
            pagination,
            supplied(query.status.as_deref()),
            supplied(query.search.as_deref()),
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
    let metadata = body.metadata.as_ref().map(Metadata::parse).transpose()?;

    let doc = svc
        .update_document(
            auth.tenant_id,
            document_id,
            auth.user_id,
            DocumentUpdate {
                title,
                status: body.status.as_deref(),
                metadata,
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
