use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::document::DocumentVersion;
use crate::error::{AppError, AppResult};
use crate::repository::tenant_context::begin_tenant_tx;

/// Create a new version snapshot for a document.
pub async fn create_version(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
    created_by: Uuid,
    comment: Option<&str>,
    is_auto: bool,
) -> AppResult<DocumentVersion> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    // Get the document's current version and yjs_state
    let row = sqlx::query(
        "SELECT current_version, yjs_state FROM editor.documents WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(document_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Document not found".to_string()))?;

    let current_version: i32 = row.get("current_version");
    let yjs_state: Option<Vec<u8>> = row.get("yjs_state");
    let yjs_snapshot = yjs_state.unwrap_or_default();
    let new_version = current_version + 1;
    let snapshot_size = yjs_snapshot.len() as i32;

    // Insert the version
    let version: DocumentVersion = sqlx::query_as(
        r#"INSERT INTO editor.document_versions
            (document_id, tenant_id, version, yjs_snapshot, created_by, comment, snapshot_size_bytes, is_auto)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id, document_id, tenant_id, version, yjs_snapshot, created_by,
                   created_at, comment, snapshot_size_bytes, is_auto"#,
    )
    .bind(document_id)
    .bind(tenant_id)
    .bind(new_version)
    .bind(&yjs_snapshot)
    .bind(created_by)
    .bind(comment)
    .bind(snapshot_size)
    .bind(is_auto)
    .fetch_one(&mut *tx)
    .await?;

    // Update document's current_version
    sqlx::query("UPDATE editor.documents SET current_version = $1, updated_at = now() WHERE id = $2")
        .bind(new_version)
        .bind(document_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await.map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    Ok(version)
}

/// List all versions of a document.
pub async fn list_versions(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
) -> AppResult<Vec<DocumentVersion>> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    // Verify document exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM editor.documents WHERE id = $1 AND deleted_at IS NULL)",
    )
    .bind(document_id)
    .fetch_one(&mut *tx)
    .await?;

    if !exists {
        return Err(AppError::NotFound("Document not found".to_string()));
    }

    let versions: Vec<DocumentVersion> = sqlx::query_as(
        r#"SELECT id, document_id, tenant_id, version, yjs_snapshot, created_by,
                  created_at, comment, snapshot_size_bytes, is_auto
         FROM editor.document_versions
         WHERE document_id = $1
         ORDER BY version DESC"#,
    )
    .bind(document_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await.map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    Ok(versions)
}

/// Get a specific version of a document.
pub async fn get_version(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
    version_number: i32,
) -> AppResult<DocumentVersion> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    let version: Option<DocumentVersion> = sqlx::query_as(
        r#"SELECT id, document_id, tenant_id, version, yjs_snapshot, created_by,
                  created_at, comment, snapshot_size_bytes, is_auto
         FROM editor.document_versions
         WHERE document_id = $1 AND version = $2"#,
    )
    .bind(document_id)
    .bind(version_number)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await.map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    version.ok_or_else(|| AppError::NotFound("Version not found".to_string()))
}
