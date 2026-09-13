use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::document::Document;
use crate::error::{AppError, AppResult};
use crate::repository::tenant_context::begin_tenant_tx;

/// Create a new document and return it.
pub async fn create_document(
    pool: &PgPool,
    tenant_id: Uuid,
    title: &str,
    created_by: Uuid,
    metadata: serde_json::Value,
) -> AppResult<Document> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    let doc: Document = sqlx::query_as(
        r#"INSERT INTO editor.documents (tenant_id, title, status, created_by, metadata)
        VALUES ($1, $2, 'draft', $3, $4)
        RETURNING id, tenant_id, title, status, created_by, created_at, updated_at,
                  deleted_at, current_version, word_count, metadata"#,
    )
    .bind(tenant_id)
    .bind(title)
    .bind(created_by)
    .bind(&metadata)
    .fetch_one(&mut *tx)
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
) -> AppResult<(Vec<Document>, i64)> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;
    let offset = (page - 1) * per_page;

    // Base WHERE always includes tenant_id (defense-in-depth, not just RLS)
    let base_count =
        "SELECT COUNT(*)::bigint FROM editor.documents WHERE tenant_id = $1 AND deleted_at IS NULL";
    let base_rows = "SELECT id, tenant_id, title, status, created_by, created_at, updated_at, deleted_at, current_version, word_count, metadata FROM editor.documents WHERE tenant_id = $1 AND deleted_at IS NULL";

    // Build dynamic query for count (no ORDER BY, no LIMIT/OFFSET)
    let (count_sql, count_args) = build_list_query(base_count, status_filter, search, false);

    let total: (i64,) = {
        let mut q = sqlx::query_as(&count_sql);
        q = q.bind(tenant_id);
        for arg in &count_args {
            q = q.bind(arg.as_str());
        }
        q.fetch_one(&mut *tx).await?
    };

    // Build dynamic query for rows (with ORDER BY + LIMIT/OFFSET via bind)
    let (rows_sql, rows_args) = build_list_query(base_rows, status_filter, search, true);

    // Append LIMIT/OFFSET with parameterized binds
    let param_idx_after_args = 2 + rows_args.len(); // $1=tenant_id, then dynamic args
    let limit_param = format!("${}", param_idx_after_args);
    let offset_param = format!("${}", param_idx_after_args + 1);
    let rows_sql = format!("{rows_sql} LIMIT {limit_param} OFFSET {offset_param}");

    let docs: Vec<Document> = {
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
        sql.push_str(&format!(
            " AND to_tsvector('english', title) @@ plainto_tsquery('english', ${param_idx})"
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

    let doc: Option<Document> = sqlx::query_as(
        r#"SELECT id, tenant_id, title, status, created_by, created_at, updated_at,
                  deleted_at, current_version, word_count, metadata
         FROM editor.documents
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL"#,
    )
    .bind(document_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;
    doc.ok_or_else(|| AppError::NotFound("Document not found".to_string()))
}

/// Update document metadata (title, status, metadata fields).
/// Defense-in-depth: filters by both id AND tenant_id.
pub async fn update_document(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
    title: Option<&str>,
    status: Option<&str>,
    metadata: Option<serde_json::Value>,
) -> AppResult<Document> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    // First fetch the current document to validate transitions
    let current: Option<Document> = sqlx::query_as(
        r#"SELECT id, tenant_id, title, status, created_by, created_at, updated_at,
                  deleted_at, current_version, word_count, metadata
         FROM editor.documents
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL"#,
    )
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

    let doc: Document = sqlx::query_as(
        r#"UPDATE editor.documents
         SET title = $1, status = $2, metadata = $3, updated_at = now()
         WHERE id = $4 AND tenant_id = $5 AND deleted_at IS NULL
         RETURNING id, tenant_id, title, status, created_by, created_at, updated_at,
                   deleted_at, current_version, word_count, metadata"#,
    )
    .bind(final_title)
    .bind(final_status)
    .bind(&final_metadata)
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await?;

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
