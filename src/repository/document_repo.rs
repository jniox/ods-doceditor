use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::document::{word_count, Document, DocumentSummary, DocumentUpdate};
use crate::domain::metadata::Metadata;
use crate::domain::pagination::Pagination;
use crate::domain::text::Title;
use crate::error::{AppError, AppResult};
use crate::repository::tenant_context::begin_tenant_tx;
use crate::repository::version_repo::{insert_version, NewVersion, VersionBody};

/// How much of a document PostgreSQL is asked to index for full-text search.
///
/// Not a performance knob: a `tsvector` cannot hold more than 1 048 575 bytes
/// of lexemes, and an index expression that raises makes the **write** raise.
/// Until migration 009 the index covered `title || ' ' || content` whole, so
/// whether a document could be stored at all depended on the *vocabulary* of
/// its body rather than on its size — measured on PostgreSQL 17: a body of
/// 798 893 bytes of distinct reference codes was refused while 10 050 000 bytes
/// of ordinary repetitive prose went in, against a `MAX_DOCUMENT_SIZE_MB` that
/// said 10 MB at the time and says 2 MB since HR-20260914-001. Lowering the
/// ceiling does not make this bound redundant: it is what keeps the storable
/// size independent of where the ceiling moves next.
///
/// Characters and not bytes, because PostgreSQL's `left()` counts characters.
/// The worst case measured for this value — 250 000 characters of distinct
/// accented tokens — produces 576 628 bytes of `tsvector`, a little over half
/// the hard limit. See `tests/search_index_test.rs`, which re-measures it in
/// three alphabets rather than trusting this paragraph.
pub const INDEXED_PREFIX_CHARS: usize = 250_000;

/// The searchable projection of a document, named once.
///
/// Migration 009 defines `editor.searchable_text(title, content)` as the first
/// [`INDEXED_PREFIX_CHARS`] characters of the title followed by the body, and
/// builds the GIN index on it. The predicate below goes through the same
/// function, which is what makes the two agree: an inlined copy would still
/// answer, but it would stop matching the index — and a sequential scan would
/// then evaluate `to_tsvector` over the untruncated body and raise, on a read,
/// the error this batch removed from the write.
pub const SEARCHABLE_TEXT: &str = "editor.searchable_text(title, content)";

/// The full-text predicate, built in one place so the tests measure the same
/// SQL the service sends.
pub fn search_predicate(param_placeholder: &str) -> String {
    format!(
        "to_tsvector('english', {SEARCHABLE_TEXT}) @@ \
         plainto_tsquery('english', {param_placeholder})"
    )
}

/// Full document projection, body included.
pub const DOC_COLUMNS: &str = "id, tenant_id, title, status, created_by, created_at, updated_at, \
                               deleted_at, current_version, word_count, metadata, content";

/// What an update reads under its lock — and deliberately not one column more.
///
/// [`update_document`] locks the row before it decides anything, and until
/// 2026-09-15 it locked it with [`DOC_COLUMNS`]: every PATCH therefore pulled
/// the whole body out of PostgreSQL, *including* a PATCH whose only purpose was
/// to replace it. The lock needs the current `status`, to judge the transition,
/// and the existence of a live row, to answer 404. Nothing else.
///
/// Named as a constant for the same reason `version_repo` names its two
/// projections apart: a projection that is written inline is a projection that
/// grows a body column back the next time someone needs one more field.
pub const UPDATE_LOCK_COLUMNS: &str = "status";

/// List projection: everything except the body (see `DocumentSummary`).
const SUMMARY_COLUMNS: &str = "id, tenant_id, title, status, created_by, created_at, updated_at, \
                               deleted_at, current_version, word_count, metadata";

/// Create a new document, with its body, and seed version 1.
///
/// The version row is written in the same transaction as the document: a
/// document whose history starts at version 2 (which is what happened before
/// this batch) makes "restore the original" impossible for every product.
///
/// The title arrives as a parsed [`Title`] rather than as a `&str`, and so does
/// the one in [`DocumentUpdate`]: this module is the only writer of a
/// `VARCHAR(500)` column, so requiring the parsed value here is what makes "a
/// title is trimmed and at most 500 characters" true of every path, present and
/// future, instead of true of whichever caller remembered. See
/// [`crate::domain::text`].
///
/// [`Metadata`] is here for the same reason and it is the newer of the two: the
/// `jsonb` column had no bound at all beyond the payload ceiling, and this
/// module is its only writer. See [`crate::domain::metadata`].
pub async fn create_document(
    pool: &PgPool,
    tenant_id: Uuid,
    title: &Title,
    created_by: Uuid,
    metadata: &Metadata,
    content: &str,
) -> AppResult<Document> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    let doc: Document = sqlx::query_as(&format!(
        r#"INSERT INTO editor.documents (tenant_id, title, status, created_by, metadata, content, word_count)
        VALUES ($1, $2, 'draft', $3, $4, $5, $6)
        RETURNING {DOC_COLUMNS}"#
    ))
    .bind(tenant_id)
    .bind(title.as_str())
    .bind(created_by)
    .bind(metadata.as_value())
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
            // The caller sent this body; it is already in this process, so
            // binding it costs nothing the request has not already paid.
            body: VersionBody::Supplied {
                content,
                yjs_snapshot: &[],
            },
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
///
/// The page arrives already normalised (see [`Pagination`]) rather than as two
/// loose integers: `(page - 1) * per_page` used to be computed here from
/// whatever the query string carried, and `page=9223372036854775807` overflowed
/// it — a panic on a debug build, and a wrap to `OFFSET -200` on a release one,
/// which PostgreSQL refuses outright.
pub async fn list_documents(
    pool: &PgPool,
    tenant_id: Uuid,
    pagination: Pagination,
    status_filter: Option<&str>,
    search: Option<&str>,
) -> AppResult<(Vec<DocumentSummary>, i64)> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;
    let offset = pagination.offset();

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
        q = q.bind(pagination.per_page());
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
        // The body is searchable too, now that there is one — through the same
        // named projection the index is built on (migration 009). Inlining
        // `title || ' ' || content` here is what used to make a large document
        // unstorable, and would now make a large document unsearchable-by-500.
        sql.push_str(&format!(
            " AND {}",
            search_predicate(&format!("${param_idx}"))
        ));
        args.push(q.to_string());
        // param_idx += 1; // not needed further but included for correctness
        let _ = param_idx; // suppress unused warning
    }

    if add_order {
        // A total order, and `id` is not decoration. `updated_at` alone ties
        // whenever two documents are written by one transaction — a seed, an
        // import, a data migration — and PostgreSQL is then free to return
        // tied rows in whatever order the plan it picked produces. It does not
        // pick the same plan for every page: measured on 2026-09-14 over 2 000
        // tied documents, `OFFSET 0` ran an `Index Scan using
        // idx_documents_tenant_updated` and `OFFSET 1900` a `Sort`, and the
        // two orders differ by a rotation. Walking the twenty pages of that
        // list returned 2 000 rows holding **1 999** documents: one returned
        // twice, one never returned at all.
        //
        // A client that walks the pages to build its own list silently loses a
        // document, and the response says nothing — the same class as the page
        // this endpoint used to report having served. `id` is the primary key,
        // so appending it makes the order total and the walk a partition,
        // whatever plan each page gets. See tests/list_stability_test.rs.
        sql.push_str(" ORDER BY updated_at DESC, id DESC");
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

    // Lock the row, and read of it only what the decision needs: the current
    // status, to judge the transition, and the existence of a live row, to
    // answer 404. See [`UPDATE_LOCK_COLUMNS`] — this read used to ask for
    // `DOC_COLUMNS`, so every PATCH pulled the whole body out of PostgreSQL,
    // including a PATCH whose only purpose was to replace it.
    //
    // `FOR UPDATE` is what makes the version number safe to advance: the lock
    // and the write below are one read-modify-write on `current_version`, and
    // without the lock two concurrent editors claim the same number and the
    // `UNIQUE (document_id, version)` of migration 003 turns the loser into a
    // 500 on a legitimate save — while a save racing an explicit snapshot
    // deadlocks, the two paths writing the same two tables in opposite orders.
    // Two editors on one document is this service's normal traffic, not an edge
    // case. It stays here, taken on the same row and in the same order as
    // `version_repo::create_version`. See ADR-004 and tests/concurrency_test.rs.
    let current: Option<(String,)> = sqlx::query_as(&format!(
        r#"SELECT {UPDATE_LOCK_COLUMNS}
         FROM editor.documents
         WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL
         FOR UPDATE"#
    ))
    .bind(document_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (current_status,) =
        current.ok_or_else(|| AppError::NotFound("Document not found".to_string()))?;

    // Validate status transition if status is being changed
    if let Some(new_status) = status {
        use crate::domain::document::DocumentStatus;
        let from = DocumentStatus::parse(&current_status)
            .ok_or_else(|| AppError::Internal("Invalid current status".to_string()))?;
        let target_status = DocumentStatus::parse(new_status)
            .ok_or_else(|| AppError::BadRequest(format!("Invalid status: {new_status}")))?;

        if !from.can_transition_to(&target_status) {
            return Err(AppError::BadRequest(format!(
                "Cannot transition from '{current_status}' to '{new_status}'"
            )));
        }
    }

    // One statement, and every column the caller did not name keeps the value
    // it already has — `COALESCE($n, column)` rather than a value read a
    // statement earlier and written straight back.
    //
    // The difference is not style. A column bound to a parameter is a column
    // PostgreSQL must store afresh, so binding the old body back re-TOASTed it:
    // new chunks, new write-ahead log, and the previous chunks dead until
    // autovacuum — measured at 9 087 272 bytes of WAL to change a title on an
    // 8 MB document, against 299 120 for the same rename written this way.
    // Passing the column through leaves the TOAST pointer untouched, which is
    // what makes a rename cost the size of a title again.
    //
    // `word_count` and `current_version` move with the body and only with it,
    // which is why they are `CASE`s on the same parameter rather than values
    // computed in Rust from a row read under the lock.
    let doc: Document = sqlx::query_as(&format!(
        r#"UPDATE editor.documents
         SET title = COALESCE($1, title),
             status = COALESCE($2, status),
             metadata = COALESCE($3, metadata),
             content = COALESCE($4, content),
             word_count = CASE WHEN $4 IS NULL THEN word_count ELSE $5 END,
             current_version = CASE WHEN $4 IS NULL
                                    THEN current_version
                                    ELSE current_version + 1 END,
             updated_at = now()
         WHERE id = $6 AND tenant_id = $7 AND deleted_at IS NULL
         RETURNING {DOC_COLUMNS}"#
    ))
    .bind(title.as_ref().map(Title::as_str))
    .bind(status)
    .bind(metadata.map(Metadata::into_value))
    .bind(content)
    .bind(content.map(word_count))
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(content) = content {
        insert_version(
            &mut tx,
            tenant_id,
            document_id,
            NewVersion {
                // The number PostgreSQL just assigned under the lock, not one
                // recomputed here: two spellings of "the next version" is how
                // the document row and its history come to disagree.
                version: doc.current_version,
                // Supplied, like creation: this body arrived in the request.
                body: VersionBody::Supplied {
                    content,
                    yjs_snapshot: &[],
                },
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
