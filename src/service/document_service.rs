use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::config::DEFAULT_MAX_DOCUMENT_BYTES;
use crate::domain::document::{
    Document, DocumentStatus, DocumentSummary, DocumentUpdate, DocumentVersion,
    DocumentVersionSummary,
};
use crate::domain::metadata::Metadata;
use crate::domain::pagination::Pagination;
use crate::domain::text::{nul_at, nul_refusal, Comment, Title};
use crate::error::{AppError, AppResult};
use crate::events::producer::{CloudEvent, EventProducer};
use crate::repository::{document_repo, template_repo, version_repo};

#[derive(Clone)]
pub struct DocumentService {
    pool: PgPool,
    producer: Arc<dyn EventProducer>,
    max_content_bytes: usize,
}

impl DocumentService {
    /// A service bounded by the default body ceiling.
    ///
    /// The number is read from `config` and not restated here: two independent
    /// spellings of the same default is how a ceiling comes to be lowered in
    /// one place and left standing in the other — which is what this
    /// constructor did until 2026-09-14, carrying its own `10 * 1024 * 1024`
    /// beside `config.rs`'s own `10`. See
    /// [`crate::config::DEFAULT_MAX_DOCUMENT_SIZE_MB`] for the measurement
    /// behind the value and for HR-20260914-001, which settled it.
    pub fn new(pool: PgPool, producer: Arc<dyn EventProducer>) -> Self {
        Self {
            pool,
            producer,
            max_content_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
        }
    }

    /// Override the body ceiling (wired from `MAX_DOCUMENT_SIZE_MB`).
    ///
    /// The HTTP payload limit alone is not enough: it answers with a framework
    /// error on the whole request, whereas a product needs to know that *the
    /// body* is what it must shrink.
    pub fn with_max_content_bytes(mut self, max_content_bytes: usize) -> Self {
        self.max_content_bytes = max_content_bytes;
        self
    }

    /// Create a new document (AC-001), optionally from a template (AC-018).
    pub async fn create_document(
        &self,
        tenant_id: Uuid,
        title: &str,
        created_by: Uuid,
        metadata: &Metadata,
        content: Option<&str>,
        template_id: Option<Uuid>,
    ) -> AppResult<Document> {
        // BR-001: the title is parsed, not merely checked — what is validated
        // is what `document_repo` then stores. See `domain::text`.
        let title = Title::parse(title)?;

        // A body and a template are two answers to the same question. Picking
        // one silently is how `template_id` became a field clients could send
        // and the service could ignore.
        let content = match (content, template_id) {
            (Some(_), Some(_)) => {
                return Err(AppError::BadRequest(
                    "content and template_id are mutually exclusive".to_string(),
                ))
            }
            (_, Some(template_id)) => {
                template_repo::get_template(&self.pool, tenant_id, template_id)
                    .await?
                    .content_html
            }
            (Some(content), None) => content.to_string(),
            (None, None) => String::new(),
        };

        self.validate_content(&content)?;

        let doc = document_repo::create_document(
            &self.pool, tenant_id, &title, created_by, metadata, &content,
        )
        .await?;

        let event = CloudEvent::document_created(tenant_id, doc.id, &doc.title, created_by);
        self.publish(event);
        // Version 1 is a real row; announcing it keeps version depth derivable
        // from the event stream alone, with no special case for creation.
        let event =
            CloudEvent::version_created(tenant_id, doc.id, doc.current_version, created_by, true);
        self.publish(event);

        Ok(doc)
    }

    /// List documents for tenant (AC-002). Bodies are not included; fetch the
    /// document itself for those.
    ///
    /// The page arrives normalised from the boundary as a [`Pagination`], so
    /// this layer no longer clamps numbers the response layer would then report
    /// unclamped — the split that made `?per_page=1000` answer
    /// `"per_page": 1000` above at most a hundred documents.
    ///
    /// `status`, on the other hand, is validated here and refused when it is
    /// not a [`DocumentStatus`]: an unknown word used to filter nothing out and
    /// answer `200` with an empty page, which tells a client with a typo that it
    /// owns no documents. `PATCH` has always answered `400` for the same word.
    pub async fn list_documents(
        &self,
        tenant_id: Uuid,
        pagination: Pagination,
        status: Option<&str>,
        search: Option<&str>,
    ) -> AppResult<(Vec<DocumentSummary>, i64)> {
        if let Some(status) = status {
            if DocumentStatus::parse(status).is_none() {
                return Err(AppError::BadRequest(format!("Invalid status: {status}")));
            }
        }
        document_repo::list_documents(&self.pool, tenant_id, pagination, status, search).await
    }

    /// Get a single document, body included.
    pub async fn get_document(&self, tenant_id: Uuid, document_id: Uuid) -> AppResult<Document> {
        document_repo::get_document(&self.pool, tenant_id, document_id).await
    }

    /// Update a document (AC-003, AC-004, AC-019).
    pub async fn update_document(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
        user_id: Uuid,
        update: DocumentUpdate<'_>,
    ) -> AppResult<Document> {
        let DocumentUpdate {
            title,
            status,
            metadata,
            content,
        } = update;
        // The title needs no check here: it is a `Title`, so it was trimmed and
        // bounded when it was built. This layer used to validate `t.trim()` and
        // hand the untrimmed `t` to the repository one call below — a rename to
        // a 500-character title with a leading space became `22001 value too
        // long` inside `VARCHAR(500)`, and a `500` for the caller.

        // `metadata` needs no check here either: it is a `Metadata`, so it was
        // bounded — in keys, in characters at every depth, and in serialised
        // size — when it was built at the boundary. See `domain::metadata`.

        if let Some(c) = content {
            self.validate_content(c)?;
        }

        let has_metadata = metadata.is_some();
        let has_title = title.is_some();

        let doc = document_repo::update_document(
            &self.pool,
            tenant_id,
            document_id,
            user_id,
            DocumentUpdate {
                title,
                status,
                metadata,
                content,
            },
        )
        .await?;

        // Determine changes and emit event
        let mut changes = Vec::new();
        if content.is_some() {
            changes.push("content");
            let event = CloudEvent::version_created(
                tenant_id,
                document_id,
                doc.current_version,
                user_id,
                true,
            );
            self.publish(event);
        }
        if has_title {
            changes.push("title");
        }
        if status.is_some() {
            changes.push("status");
            // If status changed to published, emit published event
            if status == Some("published") {
                let event = CloudEvent::document_published(
                    tenant_id,
                    document_id,
                    user_id,
                    doc.current_version,
                );
                self.publish(event);
            }
        }
        if has_metadata {
            changes.push("metadata");
        }

        if !changes.is_empty() {
            let event = CloudEvent::document_updated(
                tenant_id,
                document_id,
                user_id,
                changes,
                doc.current_version,
            );
            self.publish(event);
        }

        Ok(doc)
    }

    /// Soft-delete a document (AC-021).
    pub async fn delete_document(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
        deleted_by: Uuid,
    ) -> AppResult<()> {
        document_repo::delete_document(&self.pool, tenant_id, document_id).await?;

        let event = CloudEvent::document_deleted(tenant_id, document_id, deleted_by);
        self.publish(event);

        Ok(())
    }

    /// Create an explicit version snapshot (AC-007).
    ///
    /// BR-022 (the comment's bound) is applied here rather than in the handler:
    /// the rule belongs to the one layer every caller of this service crosses,
    /// and `version_repo` will only take the parsed value anyway.
    pub async fn create_version(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
        created_by: Uuid,
        comment: Option<&str>,
    ) -> AppResult<DocumentVersionSummary> {
        let comment = comment.map(Comment::parse).transpose()?;
        let version = version_repo::create_version(
            &self.pool,
            tenant_id,
            document_id,
            created_by,
            comment.as_ref(),
            false,
        )
        .await?;

        let event =
            CloudEvent::version_created(tenant_id, document_id, version.version, created_by, false);
        self.publish(event);

        Ok(version)
    }

    /// List versions for a document.
    ///
    /// Summaries, never bodies: the history is unpaginated by contract, so a
    /// projection that carried the bodies cost the whole of a document's
    /// history in memory for a response that contains none of it. See
    /// [`DocumentVersionSummary`] and `tests/history_read_test.rs`.
    pub async fn list_versions(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
    ) -> AppResult<Vec<DocumentVersionSummary>> {
        version_repo::list_versions(&self.pool, tenant_id, document_id).await
    }

    /// Get a specific version.
    pub async fn get_version(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
        version_number: i32,
    ) -> AppResult<DocumentVersion> {
        version_repo::get_version(&self.pool, tenant_id, document_id, version_number).await
    }

    /// Publish an event, reporting a failure instead of discarding it.
    ///
    /// Publication is best-effort by design — an analytics event must never
    /// fail a document write — but "best effort" and "silently ignored" are
    /// different things, and only one of them is debuggable.
    fn publish(&self, event: CloudEvent) {
        // One chokepoint, so no event can be emitted without its correlation.
        let event = match crate::correlation::current() {
            Some(correlation_id) => event.with_correlation_id(correlation_id),
            None => event,
        };
        if let Err(e) = self.producer.publish(event) {
            tracing::error!("Failed to publish an editor event: {e}");
        }
    }

    /// Enforce what a body must satisfy to be *stored*: the configured ceiling,
    /// and the one character the column cannot hold.
    ///
    /// The ceiling is in bytes and not characters — the storage cost is bytes,
    /// and a body of accented text would otherwise pass a character check and
    /// fail at the payload limit.
    ///
    /// `U+0000` is refused here rather than by PostgreSQL, which used to answer
    /// it with `22021` and therefore `500 internal_error` on a body a caller
    /// could perfectly well send. This is the only field the service does not
    /// parse into a domain value — the contract stores it verbatim — so the
    /// rule is applied where the body is checked. See
    /// [`crate::domain::text::nul_at`] and `tests/unstorable_character_test.rs`.
    fn validate_content(&self, content: &str) -> AppResult<()> {
        if content.len() > self.max_content_bytes {
            return Err(AppError::Validation(format!(
                "Content exceeds the maximum document size of {} bytes",
                self.max_content_bytes
            )));
        }
        if let Some(at) = nul_at(content) {
            return Err(AppError::Validation(nul_refusal("Content", at)));
        }
        Ok(())
    }
}
