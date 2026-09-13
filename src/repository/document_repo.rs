use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::document::{word_count, Document, DocumentSummary, DocumentUpdate};
use crate::error::{AppError, AppResult};
use crate::repository::tenant_context::begin_tenant_tx;
use crate::repository::version_repo::{insert_version, NewVersion};

/// Full document projection, body included.
const DOC_COLUMNS: &str = "id, tenant_id, title, status, created_by, created_at, updated_at, \
                           deleted_at, current_version, word_count, metadata, content";

/// List projection: everything except the body (see `DocumentSummary`).
const SUMMARY_COLUMNS: &str = "id, tenant_id, title, status, created_by, created_at, updated_at, \
                               deleted_at, current_version, word_count, metadata";

/// Create a new document, with its body, and seed version 1.
///
/// The version row is written in the same transaction as the document: a
/// document whose history starts at version 2 (which is what happened before
/// this batch) makes "restore the original" impossible for every product.
pub async fn create_document(
    pool: &PgPool,
    tenant_id: Uuid,
    title: &str,
    created_by: Uuid,
    metadata: serde_json::Value,
    content: &str,
) -> AppResult<Document> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    let doc: Document = sqlx::query_as(&format!(
        r#"INSERT INTO editor.documents (tenant_id, title, status, created_by, metadata, content, word_count)
        VALUES ($1, $2, 'draft', $3, $4, $5, $6)
        RETURNING {DOC_COLUMNS}"#
    ))
    .bind(tenant_id)
    .bind(title)
    .bind(created_by)
    .bind(&metadata)
    .bind(content)
    .bind(word_count(content))
    .fetch_one(&mut *tx)
    .await?;

    // `is_auto = true`: the service took this snapshot, no product asked for it.
    insert_version(
        &mut tx,
        tenant_id,
        doc.id,
        NewVersion {
            version: doc.current_version,
            content,
            yjs_snapshot: &[],
            created_by,
            comment: Some("initial version"),
            is_auto: true,
        },
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    Ok(doc)
}

/// List documents for a tenant with pagination and optional status filter.
pub async fn list_documents(
    pool: &PgPool,
    tenant_id: Uuid,
    page: i64,
    per_page: i64,
    status_filter: Option<&str>,
    search: Option<&str>,
) -> AppResult<(Vec<DocumentSummary>, i64)> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;
    let offset = (page - 1) * per_page;

    // Base WHERE always includes tenant_id (defense-in-depth, not just RLS)
    let base_count =
        "SELECT COUNT(*)::bigint FROM editor.documents WHERE tenant_id = $1 AND deleted_at IS NULL"
            .to_string();
    let base_rows = format!(
        "SELECT {SUMMARY_COLUMNS} FROM editor.documents WHERE tenant_id = $1 AND deleted_at IS NULL"
    );

    // Build dynamic query for count (no ORDER BY, no LIMIT/OFFSET)
    let (count_sql, count_args) = build_list_query(&base_count, status_filter, search, false);

    let total: (i64,) = {
        let mut q = sqlx::query_as(&count_sql);
        q = q.bind(tenant_id);
        for arg in &count_args {
            q = q.bind(arg.as_str());
        }
        q.fetch_one(&mut *tx).await?
    };

    // Build dynamic query for rows (with ORDER BY + LIMIT/OFFSET via bind)
    let (rows_sql, rows_args) = build_list_query(&base_rows, status_filter, search, true);

    // Append LIMIT/OFFSET with parameterized binds
    let param_idx_after_args = 2 + rows_args.len(); // $1=tenant_id, then dynamic args
    let limit_param = format!("${}", param_idx_after_args);
    let offset_param = format!("${}", param_idx_after_args + 1);
    let rows_sql = format!("{rows_sql} LIMIT {limit_param} OFFSET {offset_param}");

    let docs: Vec<DocumentSummary> = {
        let mut q = sqlx::query_as(&rows_sql);
        q = q.bind(tenant_id);
        for arg in &rows_args {
            q = q.bind(arg.as_str());
        }
        q = q.bind(per_page);
        q = q.bind(offset);
        q.fetch_all(&mut *tx).await?
    };

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    Ok((docs, total.0))
}

fn build_list_query(
    base: &str,
    status_filter: Option<&str>,
    search: Option<&str>,
    add_order: bool,
) -> (String, Vec<String>) {
    let mut sql = base.to_string();
    let mut args: Vec<String> = Vec::new();
    // Start at 2 because $1 is always tenant_id
    let mut param_idx = 2;

    if let Some(status) = status_filter {
        sql.push_str(&format!(" AND status = ${param_idx}"));
        args.push(status.to_string());
        param_idx += 1;
    }

    if let Some(q) = search {
        // The body is searchable too, now that there is one.
        sql.push_str(&format!(
            " AND to_tsvector('english', title || ' ' || content) @@ plainto_tsquery('english', ${param_idx})"
        ));
        args.push(q.to_string());
        // param_idx += 1; // not needed further but included for correctness
        let _ = param_idx; // suppress unused warning
    }

    if add_order {
        sql.push_str(" ORDER BY updated_at DESC");
    }

    (sql, args)
}

/// Get a single document by ID (within tenant context).
/// Defense-in-depth: filters by both id AND tenant_id.
pub async fn get_document(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
) -> AppResult<Document> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    let doc: Option<Document> = sqlx::query_as(&format!(
        r#"SELECT {DOC_COLUMNS}
         FROM editor.documents
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL"#
    ))
    .bind(document_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    doc.ok_or_else(|| AppError::NotFound("Document not found".to_string()))
}

/// Update a document (title, status, metadata and/or body).
///
/// A body change advances `current_version` and writes the matching immutable
/// version row in the same transaction — the GTM contract is that every content
/// mutation leaves a restorable record, which cannot be the caller's
/// responsibility without losing the guarantee.
///
/// Defense-in-depth: filters by both id AND tenant_id.
pub async fn update_document(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
    updated_by: Uuid,
    update: DocumentUpdate<'_>,
) -> AppResult<Document> {
    let DocumentUpdate {
        title,
        status,
        metadata,
        content,
    } = update;
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    // First fetch the current document to validate transitions
    let current: Option<Document> = sqlx::query_as(&format!(
        r#"SELECT {DOC_COLUMNS}
         FROM editor.documents
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL"#
    ))
    .bind(document_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    let current = current.ok_or_else(|| AppError::NotFound("Document not found".to_string()))?;

    // Validate status transition if status is being changed
    if let Some(new_status) = status {
        use crate::domain::document::DocumentStatus;
        let current_status = DocumentStatus::parse(&current.status)
            .ok_or_else(|| AppError::Internal("Invalid current status".to_string()))?;
        let target_status = DocumentStatus::parse(new_status)
            .ok_or_else(|| AppError::BadRequest(format!("Invalid status: {new_status}")))?;

        if !current_status.can_transition_to(&target_status) {
            return Err(AppError::BadRequest(format!(
                "Cannot transition from '{}' to '{}'",
                current.status, new_status
            )));
        }
    }

    let final_title = title.unwrap_or(&current.title);
    let final_status = status.unwrap_or(&current.status);
    let final_metadata = metadata.unwrap_or(current.metadata.clone());
    let final_content = content.unwrap_or(&current.content);
    let final_version = match content {
        Some(_) => current.current_version + 1,
        None => current.current_version,
    };

    let doc: Document = sqlx::query_as(&format!(
        r#"UPDATE editor.documents
         SET title = $1, status = $2, metadata = $3, content = $4, word_count = $5,
             current_version = $6, updated_at = now()
         WHERE id = $7 AND tenant_id = $8 AND deleted_at IS NULL
         RETURNING {DOC_COLUMNS}"#
    ))
    .bind(final_title)
    .bind(final_status)
    .bind(&final_metadata)
    .bind(final_content)
    .bind(word_count(final_content))
    .bind(final_version)
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await?;

    if content.is_some() {
        insert_version(
            &mut tx,
            tenant_id,
            document_id,
            NewVersion {
                version: final_version,
                content: final_content,
                yjs_snapshot: &[],
                created_by: updated_by,
                comment: None,
                is_auto: true,
            },
        )
        .await?;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    Ok(doc)
}

/// Soft-delete a document.
/// Defense-in-depth: filters by both id AND tenant_id.
pub async fn delete_document(pool: &PgPool, tenant_id: Uuid, document_id: Uuid) -> AppResult<()> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    let result = sqlx::query(
        r#"UPDATE editor.documents
         SET deleted_at = now(), status = 'archived'
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL"#,
    )
    .bind(document_id)
    .bind(tenant_id)
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Document not found".to_string()));
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    Ok(())
}
