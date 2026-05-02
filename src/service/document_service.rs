use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::document::{Document, DocumentVersion};
use crate::error::{AppError, AppResult};
use crate::events::producer::{CloudEvent, EventProducer};
use crate::repository::{document_repo, version_repo};

#[derive(Clone)]
pub struct DocumentService {
    pool: PgPool,
    producer: Arc<dyn EventProducer>,
}

impl DocumentService {
    pub fn new(pool: PgPool, producer: Arc<dyn EventProducer>) -> Self {
        Self { pool, producer }
    }

    /// Create a new document (AC-001).
    pub async fn create_document(
        &self,
        tenant_id: Uuid,
        title: &str,
        created_by: Uuid,
        metadata: serde_json::Value,
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

        let doc = document_repo::create_document(
            &self.pool,
            tenant_id,
            trimmed,
            created_by,
            metadata,
        )
        .await?;

        // Emit event
        let event = CloudEvent::document_created(tenant_id, doc.id, &doc.title, created_by);
        let _ = self.producer.publish(event);

        Ok(doc)
    }

    /// List documents for tenant (AC-002).
    pub async fn list_documents(
        &self,
        tenant_id: Uuid,
        page: i64,
        per_page: i64,
        status: Option<&str>,
        search: Option<&str>,
    ) -> AppResult<(Vec<Document>, i64)> {
        let page = page.max(1);
        let per_page = per_page.clamp(1, 100);
        document_repo::list_documents(&self.pool, tenant_id, page, per_page, status, search).await
    }

    /// Get a single document.
    pub async fn get_document(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
    ) -> AppResult<Document> {
        document_repo::get_document(&self.pool, tenant_id, document_id).await
    }

    /// Update document metadata (AC-003, AC-004).
    pub async fn update_document(
        &self,
        tenant_id: Uuid,
        document_id: Uuid,
        user_id: Uuid,
        title: Option<&str>,
        status: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> AppResult<Document> {
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

        let has_metadata = metadata.is_some();

        let doc = document_repo::update_document(
            &self.pool, tenant_id, document_id, title, status, metadata,
        )
        .await?;

        // Determine changes and emit event
        let mut changes = Vec::new();
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
                let _ = self.producer.publish(event);
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
            let _ = self.producer.publish(event);
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
        let _ = self.producer.publish(event);

        Ok(())
    }

    /// Create a version snapshot (AC-005 versioning).
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

        let event = CloudEvent::version_created(
            tenant_id,
            document_id,
            version.version,
            created_by,
            false,
        );
        let _ = self.producer.publish(event);

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
