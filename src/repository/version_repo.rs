use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::document::{DocumentVersion, DocumentVersionSummary};
use crate::domain::text::Comment;
use crate::error::{AppError, AppResult};
use crate::repository::tenant_context::begin_tenant_tx;

/// The full projection of a version row, body included.
///
/// Used by exactly one read — `GET /documents/{id}/versions/{n}` — because that
/// is the one whose purpose is to hand a prior body back to a product.
pub const VERSION_COLUMNS: &str = "id, document_id, tenant_id, version, yjs_snapshot, \
                                   created_by, created_at, comment, snapshot_size_bytes, \
                                   is_auto, content";

/// The projection of a version row **without** its body: what the history list
/// serves, and what every write path needs back after inserting one.
///
/// Named separately from [`VERSION_COLUMNS`] for the same reason
/// `document_repo` names `SUMMARY_COLUMNS` apart from `DOC_COLUMNS`, only with
/// more at stake: a page of documents is capped at a hundred rows, a version
/// history is capped at nothing, so the cost of reading bodies nobody asked for
/// grows without bound. Measured before this split, on the 512 MiB the
/// deployment allocates: a document at the published 10 MB ceiling with 55
/// versions — an afternoon of autosaves — answered
/// `GET /documents/{id}/versions` with an **OOM kill of the process**, not with
/// a 500. The response it was building is nine kilobytes of JSON and contains
/// no body at all; `docs/openapi.yaml` says so explicitly.
///
/// The three write paths take it too. `insert_version` used to return the body
/// it had just been given, straight back out of PostgreSQL, for three callers
/// of which **none** reads it: creation and content mutation discard the value
/// entirely, and the explicit snapshot renders only the fields below.
pub const VERSION_SUMMARY_COLUMNS: &str = "id, document_id, tenant_id, version, created_by, \
                                           created_at, comment, snapshot_size_bytes, is_auto";

/// What an explicit snapshot reads under its lock — and deliberately not one
/// column more.
///
/// [`create_version`] locks the document row before it decides anything, and
/// until 2026-09-15 it locked it with `current_version, yjs_state, content`:
/// every snapshot therefore pulled the whole body out of PostgreSQL only to bind
/// it straight back into the INSERT one statement later. The lock needs the
/// number the next version follows, and the existence of a live row to answer
/// 404. Nothing else.
///
/// Named as a constant for the same reason `document_repo` names
/// `UPDATE_LOCK_COLUMNS`: a projection written inline is a projection that grows
/// a body column back the next time someone needs one more field.
pub const SNAPSHOT_LOCK_COLUMNS: &str = "current_version";

/// Where the body of the new version row comes from.
///
/// The distinction is a cost, not a style. Two of the three paths that write a
/// version were **handed** the body by the caller — creation and content
/// mutation both receive it in the request, so it is already in this process
/// and binding it costs nothing new. The third, an explicit snapshot, was not:
/// its request is a document id and at most a 500-character comment, and the
/// body it must copy is whatever the document already stores.
///
/// Naming the two cases apart is what lets the second one be copied **inside
/// PostgreSQL**. See [`VersionBody::OfDocumentAsItStands`] and ADR-015.
pub(crate) enum VersionBody<'a> {
    /// The caller already holds the body; bind it.
    Supplied {
        content: &'a str,
        yjs_snapshot: &'a [u8],
    },
    /// The body is whatever `editor.documents` holds for this document right
    /// now, and it never leaves the server.
    ///
    /// The caller must already hold the document row's `FOR UPDATE` lock — this
    /// is a read of the same row inside the same transaction, and the version
    /// number it is paired with was decided under that lock (ADR-004).
    OfDocumentAsItStands,
}

/// Insert one immutable version row inside an existing tenant transaction.
///
/// Shared by the three paths that produce a version — document creation,
/// content mutation, explicit snapshot — so that "a version is an insert, never
/// an update" holds in exactly one place. There is intentionally no update or
/// delete counterpart in this module.
pub(crate) struct NewVersion<'a> {
    pub version: i32,
    pub body: VersionBody<'a>,
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
) -> AppResult<DocumentVersionSummary> {
    let NewVersion {
        version,
        body,
        created_by,
        comment,
        is_auto,
    } = new;

    // `RETURNING` the summary and not the row: the body is written by this very
    // statement, so asking for it back doubles the cost of every save on a value
    // none of the three callers reads.
    let version: DocumentVersionSummary = match body {
        VersionBody::Supplied {
            content,
            yjs_snapshot,
        } => {
            // The snapshot is the authored body plus whatever CRDT state
            // existed; both are restored together, so both are measured
            // together. `str::len()` is bytes, which is what
            // `snapshot_size_bytes` names and what `octet_length` counts in the
            // sibling statement below — the two must agree.
            let snapshot_size = (content.len() + yjs_snapshot.len()).min(i32::MAX as usize) as i32;
            sqlx::query_as(&format!(
                r#"INSERT INTO editor.document_versions
                    (document_id, tenant_id, version, yjs_snapshot, created_by, comment,
                     snapshot_size_bytes, is_auto, content)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 RETURNING {VERSION_SUMMARY_COLUMNS}"#
            ))
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
            .await?
        }
        // The body is copied by PostgreSQL, from one table to another, without
        // ever crossing this connection. What the caller asked for is "snapshot
        // this document as it stands"; naming that in SQL is what makes the cost
        // of the request the size of the request. See ADR-015.
        VersionBody::OfDocumentAsItStands => {
            sqlx::query_as(&format!(
                r#"INSERT INTO editor.document_versions
                    (document_id, tenant_id, version, yjs_snapshot, created_by, comment,
                     snapshot_size_bytes, is_auto, content)
                 SELECT $1, $2, $3,
                        coalesce(d.yjs_state, ''::bytea),
                        $4, $5,
                        octet_length(d.content) + coalesce(octet_length(d.yjs_state), 0),
                        $6,
                        d.content
                   FROM editor.documents d
                  WHERE d.id = $1 AND d.tenant_id = $2 AND d.deleted_at IS NULL
                 RETURNING {VERSION_SUMMARY_COLUMNS}"#
            ))
            .bind(document_id)
            .bind(tenant_id)
            .bind(version)
            .bind(created_by)
            .bind(comment)
            .bind(is_auto)
            .fetch_one(&mut *conn)
            .await?
        }
    };

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
) -> AppResult<DocumentVersionSummary> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    // Lock the row, and read of it only what the decision needs: the number the
    // next version follows, and the existence of a live row to answer 404. See
    // [`SNAPSHOT_LOCK_COLUMNS`] — this read used to ask for `content` and
    // `yjs_state` as well, and hand them straight back to the INSERT below.
    //
    // `FOR UPDATE` for the same reason as in `document_repo::update_document`,
    // and it must be the SAME lock, taken on the SAME row, in the same order:
    // this path inserts the version row before updating the document while the
    // update path does the reverse, so an explicit snapshot racing a save used
    // to deadlock — each holding what the other waited for. Serialising on the
    // document row removes both the duplicate version number and the deadlock.
    // The lock has not moved; only its projection has. See
    // tests/concurrency_test.rs.
    let row = sqlx::query(&format!(
        "SELECT {SNAPSHOT_LOCK_COLUMNS} FROM editor.documents \
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL FOR UPDATE"
    ))
    .bind(document_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Document not found".to_string()))?;

    let current_version: i32 = row.get("current_version");
    let new_version = current_version + 1;

    let version = insert_version(
        &mut tx,
        tenant_id,
        document_id,
        NewVersion {
            version: new_version,
            // PostgreSQL copies the body from one table to the other under the
            // lock we already hold. It used to travel here and back — which made
            // a thirty-byte request cost the stored document, twice, and at the
            // deployment's 512 MiB it killed the instance. See ADR-015.
            body: VersionBody::OfDocumentAsItStands,
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
) -> AppResult<Vec<DocumentVersionSummary>> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    ensure_live_document(&mut tx, tenant_id, document_id).await?;

    let versions: Vec<DocumentVersionSummary> = sqlx::query_as(&format!(
        r#"SELECT {VERSION_SUMMARY_COLUMNS}
         FROM editor.document_versions
         WHERE document_id = $1 AND tenant_id = $2
         ORDER BY version DESC"#
    ))
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

    let version: Option<DocumentVersion> = sqlx::query_as(&format!(
        r#"SELECT {VERSION_COLUMNS}
         FROM editor.document_versions
         WHERE document_id = $1 AND tenant_id = $2 AND version = $3"#
    ))
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
