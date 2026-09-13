use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::document::{Document, DocumentSummary, DocumentUpdate, DocumentVersion};
use crate::error::{AppError, AppResult};
use crate::events::producer::{CloudEvent, EventProducer};
use crate::repository::{document_repo, template_repo, version_repo};

/// Default body ceiling, matching `MAX_DOCUMENT_SIZE_MB=10`.
const DEFAULT_MAX_CONTENT_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct DocumentService {
    pool: PgPool,
    producer: Arc<dyn EventProducer>,
    max_content_bytes: usize,
}

impl DocumentService {
    pub fn new(pool: PgPool, producer: Arc<dyn EventProducer>) -> Self {
        Self {
            pool,
            producer,
            max_content_bytes: DEFAULT_MAX_CONTENT_BYTES,
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
        metadata: serde_json::Value,
        content: Option<&str>,
        template_id: Option<Uuid>,
    ) -> AppResult<Document> {
        // BR-001: validate title
        let trimmed = title.trim();
        if trimmed.is_empty() || trimmed.len() > 500 {
            return Err(AppError::Validation(
                "Title must be 1-500 characters, non-blank".to_string(),
            ));
        }

        // BR-029: validate metadata keys
        Self::validate_metadata(&metadata)?;

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
            &self.pool, tenant_id, trimmed, created_by, metadata, &content,
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
    pub async fn list_documents(
        &self,
        tenant_id: Uuid,
        page: i64,
        per_page: i64,
        status: Option<&str>,
        search: Option<&str>,
    ) -> AppResult<(Vec<DocumentSummary>, i64)> {
        let page = page.max(1);
        let per_page = per_page.clamp(1, 100);
        document_repo::list_documents(&self.pool, tenant_id, page, per_page, status, search).await
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
        // Validate title if provided
        if let Some(t) = title {
            let trimmed = t.trim();
            if trimmed.is_empty() || trimmed.len() > 500 {
                return Err(AppError::Validation(
                    "Title must be 1-500 characters, non-blank".to_string(),
                ));
            }
        }

        // Validate metadata if provided
        if let Some(ref m) = metadata {
            Self::validate_metadata(m)?;
        }

        if let Some(c) = content {
            self.validate_content(c)?;
        }

        let has_metadata = metadata.is_some();

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
        if title.is_some() {
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
    pub async fn create_version(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
        created_by: Uuid,
        comment: Option<&str>,
    ) -> AppResult<DocumentVersion> {
        let version = version_repo::create_version(
            &self.pool,
            tenant_id,
            document_id,
            created_by,
            comment,
            false,
        )
        .await?;

        let event =
            CloudEvent::version_created(tenant_id, document_id, version.version, created_by, false);
        self.publish(event);

        Ok(version)
    }

    /// List versions for a document.
    pub async fn list_versions(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
    ) -> AppResult<Vec<DocumentVersion>> {
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
        if let Err(e) = self.producer.publish(event) {
            tracing::error!("Failed to publish an editor event: {e}");
        }
    }

    /// Enforce the configured body ceiling, in bytes and not characters —
    /// the storage cost is bytes, and a body of accented text would otherwise
    /// pass a character check and fail at the payload limit.
    fn validate_content(&self, content: &str) -> AppResult<()> {
        if content.len() > self.max_content_bytes {
            return Err(AppError::Validation(format!(
                "Content exceeds the maximum document size of {} bytes",
                self.max_content_bytes
            )));
        }
        Ok(())
    }

    /// Validate metadata keys per BR-029.
    fn validate_metadata(metadata: &serde_json::Value) -> AppResult<()> {
        if let Some(obj) = metadata.as_object() {
            if obj.len() > 20 {
                return Err(AppError::Validation(
                    "Metadata cannot have more than 20 keys".to_string(),
                ));
            }
            let key_re = regex_lite::Regex::new(r"^[a-z][a-z0-9_]{0,63}$").unwrap();
            for (key, value) in obj {
                if !key_re.is_match(key) {
                    return Err(AppError::Validation(format!(
                        "Metadata key '{}' must match ^[a-z][a-z0-9_]{{0,63}}$",
                        key
                    )));
                }
                if let Some(s) = value.as_str() {
                    if s.len() > 256 {
                        return Err(AppError::Validation(format!(
                            "Metadata value for key '{}' exceeds 256 chars",
                            key
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}
