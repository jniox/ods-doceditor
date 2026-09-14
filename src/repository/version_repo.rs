use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::document::DocumentVersion;
use crate::domain::text::Comment;
use crate::error::{AppError, AppResult};
use crate::repository::tenant_context::begin_tenant_tx;

/// Insert one immutable version row inside an existing tenant transaction.
///
/// Shared by the three paths that produce a version — document creation,
/// content mutation, explicit snapshot — so that "a version is an insert, never
/// an update" holds in exactly one place. There is intentionally no update or
/// delete counterpart in this module.
pub(crate) struct NewVersion<'a> {
    pub version: i32,
    pub content: &'a str,
    pub yjs_snapshot: &'a [u8],
    pub created_by: Uuid,
    /// Already bounded: the public doors of this module take a parsed
    /// [`Comment`], and the only `&str` that reaches here is this service's own
    /// `"initial version"`. `comment` is a `VARCHAR(500)` column.
    pub comment: Option<&'a str>,
    /// `true` when the service took the snapshot itself (creation, content
    /// mutation), `false` when a product explicitly asked for one.
    pub is_auto: bool,
}

pub(crate) async fn insert_version(
    conn: &mut sqlx::PgConnection,
    tenant_id: Uuid,
    document_id: Uuid,
    new: NewVersion<'_>,
) -> AppResult<DocumentVersion> {
    let NewVersion {
        version,
        content,
        yjs_snapshot,
        created_by,
        comment,
        is_auto,
    } = new;
    // The snapshot is the authored body plus whatever CRDT state existed; both
    // are restored together, so both are measured together.
    let snapshot_size = (content.len() + yjs_snapshot.len()).min(i32::MAX as usize) as i32;

    let version: DocumentVersion = sqlx::query_as(
        r#"INSERT INTO editor.document_versions
            (document_id, tenant_id, version, yjs_snapshot, created_by, comment,
             snapshot_size_bytes, is_auto, content)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id, document_id, tenant_id, version, yjs_snapshot, created_by,
                   created_at, comment, snapshot_size_bytes, is_auto, content"#,
    )
    .bind(document_id)
    .bind(tenant_id)
    .bind(version)
    .bind(yjs_snapshot)
    .bind(created_by)
    .bind(comment)
    .bind(snapshot_size)
    .bind(is_auto)
    .bind(content)
    .fetch_one(&mut *conn)
    .await?;

    Ok(version)
}

/// Create an explicit version snapshot of the document as it stands now.
///
/// `is_auto = false`: this one was asked for by a product, unlike the snapshots
/// the service takes itself on creation and on every content mutation.
pub async fn create_version(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
    created_by: Uuid,
    comment: Option<&Comment>,
    is_auto: bool,
) -> AppResult<DocumentVersion> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    // Read the body to snapshot (defense-in-depth: filter by tenant_id).
    //
    // `FOR UPDATE` for the same reason as in `document_repo::update_document`,
    // and it must be the SAME lock, taken on the SAME row, in the same order:
    // this path inserts the version row before updating the document while the
    // update path does the reverse, so an explicit snapshot racing a save used
    // to deadlock — each holding what the other waited for. Serialising on the
    // document row removes both the duplicate version number and the deadlock.
    // See tests/concurrency_test.rs.
    let row = sqlx::query(
        "SELECT current_version, yjs_state, content FROM editor.documents WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(document_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Document not found".to_string()))?;

    let current_version: i32 = row.get("current_version");
    let yjs_state: Option<Vec<u8>> = row.get("yjs_state");
    let content: String = row.get("content");
    let yjs_snapshot = yjs_state.unwrap_or_default();
    let new_version = current_version + 1;

    let version = insert_version(
        &mut tx,
        tenant_id,
        document_id,
        NewVersion {
            version: new_version,
            content: &content,
            yjs_snapshot: &yjs_snapshot,
            created_by,
            comment: comment.map(Comment::as_str),
            is_auto,
        },
    )
    .await?;

    // Update document's current_version (defense-in-depth: filter by tenant_id)
    sqlx::query("UPDATE editor.documents SET current_version = $1, updated_at = now() WHERE id = $2 AND tenant_id = $3")
        .bind(new_version)
        .bind(document_id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    Ok(version)
}

/// 404 unless `document_id` names a **live** document of this tenant.
///
/// Every read of a document's history goes through here, and that single
/// chokepoint is the fix, not a tidying of it. The defect was an asymmetry
/// between two neighbouring functions in this very file: `list_versions`
/// carried the `deleted_at IS NULL` predicate inline and `get_version`, forty
/// lines below, did not. A soft-deleted document therefore answered 404 on
/// `GET /documents/{id}` and on `GET /documents/{id}/versions`, and served its
/// **full body** on `GET /documents/{id}/versions/{n}` — the one read of the
/// three that returns content, with `n` starting at 1 and increasing by one,
/// so no knowledge of the history was needed to walk it.
///
/// Tenant isolation was never at stake (both queries filter `tenant_id`); what
/// leaked is a document the caller's own tenant had deleted. Keeping the
/// predicate in one place is what stops the next read path from being written
/// without it. See `tests/deletion_test.rs`.
async fn ensure_live_document(
    conn: &mut sqlx::PgConnection,
    tenant_id: Uuid,
    document_id: Uuid,
) -> AppResult<()> {
    // Defense-in-depth: filter by tenant_id as well as relying on RLS.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM editor.documents WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL)",
    )
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *conn)
    .await?;

    if !exists {
        return Err(AppError::NotFound("Document not found".to_string()));
    }
    Ok(())
}

/// List all versions of a document.
pub async fn list_versions(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
) -> AppResult<Vec<DocumentVersion>> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    ensure_live_document(&mut tx, tenant_id, document_id).await?;

    let versions: Vec<DocumentVersion> = sqlx::query_as(
        r#"SELECT id, document_id, tenant_id, version, yjs_snapshot, created_by,
                  created_at, comment, snapshot_size_bytes, is_auto, content
         FROM editor.document_versions
         WHERE document_id = $1 AND tenant_id = $2
         ORDER BY version DESC"#,
    )
    .bind(document_id)
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
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

    // This is the read that returns the body, so this is the read where a
    // missing liveness check actually leaks something. See
    // `ensure_live_document`.
    ensure_live_document(&mut tx, tenant_id, document_id).await?;

    let version: Option<DocumentVersion> = sqlx::query_as(
        r#"SELECT id, document_id, tenant_id, version, yjs_snapshot, created_by,
                  created_at, comment, snapshot_size_bytes, is_auto, content
         FROM editor.document_versions
         WHERE document_id = $1 AND tenant_id = $2 AND version = $3"#,
    )
    .bind(document_id)
    .bind(tenant_id)
    .bind(version_number)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    version.ok_or_else(|| AppError::NotFound("Version not found".to_string()))
}
