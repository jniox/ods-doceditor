//! The history read that transported every body it had promised not to return.
//!
//! `GET /api/v1/documents/{id}/versions` answers with version numbers, dates,
//! authors, comments and sizes. `docs/openapi.yaml` says so in as many words —
//! *"Most recent version first. **Bodies are not included.**"* — and
//! `src/api/versions.rs` renders exactly those fields.
//!
//! The SQL underneath asked for `content` anyway, for every version, and threw
//! it away one layer higher. The same rule was already stated and implemented
//! one module over: `document_repo` splits `DOC_COLUMNS` from
//! `SUMMARY_COLUMNS` and explains why in a comment — *"shipping every body in a
//! page of 100 would make the endpoint unusable for the very case pagination
//! exists for"*. The version history has no pagination at all, so it is the
//! place where that reasoning mattered most, and it is the place where the
//! projection was never split. Same shape as the `deleted_at IS NULL` predicate
//! that was written inline in `list_versions` and forgotten in `get_version`:
//! a rule honoured in one call site and absent from its neighbour.
//!
//! Measured on the running binary before this file existed, against the
//! deployment's own limits — `ops/cloudrun/doceditor.json` allocates
//! **512 MiB**, `MAX_DOCUMENT_SIZE_MB` defaults to **10**:
//!
//! ```text
//! document of 10 484 720 B, 55 versions (54 content PATCHes — an editor's
//! ordinary autosave traffic)
//!
//!   GET /api/v1/documents/{id}/versions
//!     -> Remote end closed connection without response   (0.7 s)
//!     -> systemd: Result=oom-kill, MainPID=0
//!
//! the response that request would have produced: ~9 KB of JSON, no bodies
//! ```
//!
//! and, at a size small enough to survive, the ratio that explains it:
//!
//! ```text
//! 25 versions x 1 048 560 B
//!   bytes the query read from PostgreSQL : 25 MB
//!   bytes the response carried           : 4 268
//!   process RSS across the single call   : 35 348 kB -> 51 420 kB
//! ```
//!
//! An OOM kill is not a failed request: on Cloud Run it takes the **instance**
//! down, so every other tenant's in-flight request on it fails too, and the
//! trigger is two ordinary calls by one caller.
//!
//! What the tests below assert is therefore not "the endpoint is faster" but
//! **the cost of reading a history does not grow with the bodies in it**. They
//! measure it against PostgreSQL rather than trusting the paragraph, exactly as
//! `tests/search_index_test.rs` re-measures its bound: two documents with
//! identical histories and bodies four orders of magnitude apart must cost the
//! same to list.

mod common;

use actix_web::{http::StatusCode, test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{payload, versions};
use ods_doceditor::domain::document::DocumentUpdate;
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::repository::tenant_context::begin_tenant_tx;
use ods_doceditor::repository::version_repo::{VERSION_COLUMNS, VERSION_SUMMARY_COLUMNS};
use ods_doceditor::repository::{document_repo, version_repo};
use ods_doceditor::service::document_service::DocumentService;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// One mebibyte of body per version. Well inside the published 10 MB ceiling,
/// and large enough that "the list does not read the bodies" is a claim about
/// megabytes rather than about rounding.
const LARGE_BODY: usize = 1024 * 1024;

/// A body of a few dozen bytes, for the control document.
const SMALL_BODY: usize = 64;

fn body_of(bytes: usize) -> String {
    let unit = "Les parties conviennent de ce qui suit. ";
    unit.repeat(bytes.div_ceil(unit.len()))
}

/// A document with five versions, each holding a body of `body_bytes`.
///
/// Five is deliberate: creation writes version 1, and four content mutations
/// write versions 2 to 5 — the traffic an editor produces in a few minutes.
async fn document_with_five_versions(
    pool: &PgPool,
    tenant_id: Uuid,
    user_id: Uuid,
    body_bytes: usize,
) -> Uuid {
    let body = body_of(body_bytes);
    let doc = document_repo::create_document(
        pool,
        tenant_id,
        &common::title("Contrat de prestation"),
        user_id,
        serde_json::json!({}),
        &body,
    )
    .await
    .expect("the document is created");

    for revision in 0..4 {
        let edited = format!("{body} revision {revision}");
        document_repo::update_document(
            pool,
            tenant_id,
            doc.id,
            user_id,
            DocumentUpdate {
                content: Some(&edited),
                ..Default::default()
            },
        )
        .await
        .expect("a content mutation succeeds");
    }

    doc.id
}

/// How many bytes PostgreSQL must materialise and send for `projection`, over
/// a document's whole history.
///
/// `row::text` renders the composite as the server would have to produce it,
/// detoasting every column — which is precisely what makes a body-carrying
/// projection expensive and a summary one cheap.
async fn projection_bytes(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
    projection: &str,
) -> i64 {
    let mut tx = begin_tenant_tx(pool, tenant_id)
        .await
        .expect("tenant transaction opens");

    let bytes: i64 = sqlx::query_scalar(&format!(
        "SELECT coalesce(sum(octet_length(row::text)), 0)::bigint
           FROM (SELECT {projection}
                   FROM editor.document_versions
                  WHERE document_id = $1 AND tenant_id = $2) AS row"
    ))
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await
    .expect("the projection is measurable");

    tx.commit().await.expect("the measurement commits");
    bytes
}

/// The bodies a document's history actually holds, whatever is read of them.
async fn stored_body_bytes(pool: &PgPool, tenant_id: Uuid, document_id: Uuid) -> i64 {
    let mut tx = begin_tenant_tx(pool, tenant_id)
        .await
        .expect("tenant transaction opens");
    let bytes: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(octet_length(content)), 0)::bigint
           FROM editor.document_versions WHERE document_id = $1 AND tenant_id = $2",
    )
    .bind(document_id)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await
    .expect("the stored bodies are measurable");
    tx.commit().await.expect("the measurement commits");
    bytes
}

/// The claim, measured: listing a history costs the same whether its bodies
/// weigh 320 bytes or 5 MiB.
#[actix_web::test]
async fn listing_a_history_costs_the_same_whatever_the_bodies_weigh() {
    let pool = setup_test_pool().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let small = document_with_five_versions(&pool, tenant_id, user_id, SMALL_BODY).await;
    let large = document_with_five_versions(&pool, tenant_id, user_id, LARGE_BODY).await;

    // The fixture is only worth measuring if the bodies really are far apart.
    let stored_small = stored_body_bytes(&pool, tenant_id, small).await;
    let stored_large = stored_body_bytes(&pool, tenant_id, large).await;
    assert!(
        stored_large > 4 * 1024 * 1024 && stored_small < 4096,
        "fixture: histories must differ by orders of magnitude, got {stored_small} and {stored_large}"
    );

    let read_small = projection_bytes(&pool, tenant_id, small, VERSION_SUMMARY_COLUMNS).await;
    let read_large = projection_bytes(&pool, tenant_id, large, VERSION_SUMMARY_COLUMNS).await;

    assert!(
        read_large <= 2 * read_small,
        "the history read must not grow with the bodies: {read_small} bytes for a history of \
         {stored_small} bytes of body, {read_large} bytes for one of {stored_large}. A projection \
         that carries the bodies makes GET /documents/{{id}}/versions cost the whole document \
         history in memory — measured at 512 MiB (the Cloud Run allocation), 55 versions of a \
         10 MB document, that is an OOM kill of the instance rather than a failed request."
    );
    assert!(
        read_large < 8 * 1024,
        "five summary rows must weigh kilobytes, got {read_large}"
    );
}

/// The projections, named once each, so neither can quietly grow a body column.
///
/// This is the assertion that makes the one above hold for the **write** path
/// too: `insert_version` returns the same summary, and the three paths that
/// produce a version — creation, content mutation, explicit snapshot — used to
/// read every stored body back out of PostgreSQL immediately after writing it,
/// for three callers of which none looked at it.
#[actix_web::test]
async fn the_summary_projection_names_no_body_column_and_the_full_one_does() {
    for column in ["content", "yjs_snapshot"] {
        assert!(
            !VERSION_SUMMARY_COLUMNS.contains(column),
            "the summary projection must not read `{column}`: {VERSION_SUMMARY_COLUMNS}"
        );
    }
    // The one read that exists to serve a body must still ask for it.
    assert!(
        VERSION_COLUMNS.contains("content"),
        "the full projection serves the body: {VERSION_COLUMNS}"
    );
}

/// Cheaper must not mean poorer: the list still describes every version.
#[actix_web::test]
async fn the_history_still_reports_every_version_with_its_metadata() {
    let pool = setup_test_pool().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let document_id = document_with_five_versions(&pool, tenant_id, user_id, SMALL_BODY).await;
    let comment = common::comment("checkpoint before review");
    version_repo::create_version(
        &pool,
        tenant_id,
        document_id,
        user_id,
        Some(&comment),
        false,
    )
    .await
    .expect("an explicit snapshot succeeds");

    let history = version_repo::list_versions(&pool, tenant_id, document_id)
        .await
        .expect("the history is readable");

    let numbers: Vec<i32> = history.iter().map(|v| v.version).collect();
    assert_eq!(
        numbers,
        vec![6, 5, 4, 3, 2, 1],
        "most recent first, with no gap"
    );
    assert!(
        history.iter().all(|v| v.snapshot_size_bytes > 0),
        "every version reports the size of the body it holds"
    );
    assert_eq!(
        history.iter().filter(|v| !v.is_auto).count(),
        1,
        "exactly the one snapshot a product asked for is marked explicit"
    );
    assert_eq!(
        history
            .iter()
            .find(|v| v.version == 6)
            .and_then(|v| v.comment.as_deref()),
        Some("checkpoint before review"),
        "the comment of the explicit snapshot survives"
    );
}

/// The read that *is* supposed to carry a body still does — this is the one a
/// product uses to restore a prior state.
#[actix_web::test]
async fn retrieving_one_version_still_returns_its_body() {
    let pool = setup_test_pool().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let document_id = document_with_five_versions(&pool, tenant_id, user_id, SMALL_BODY).await;

    let version = version_repo::get_version(&pool, tenant_id, document_id, 1)
        .await
        .expect("version 1 is readable");
    assert_eq!(
        version.content,
        body_of(SMALL_BODY),
        "the original body is restorable from its version"
    );
    assert_eq!(version.snapshot_size_bytes as usize, version.content.len());
}

/// From where the caller actually stands: the HTTP response of a history read
/// is kilobytes, whatever the document weighs.
#[actix_web::test]
async fn the_history_response_carries_no_body_over_http() {
    let pool = setup_test_pool().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let document_id = document_with_five_versions(&pool, tenant_id, user_id, LARGE_BODY).await;

    let service = DocumentService::new(pool.clone(), Arc::new(InMemoryProducer::new()));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(service))
            .app_data(web::Data::new(test_jwt_config()))
            .configure(payload::limits(10 * 1024 * 1024))
            .route(
                "/api/v1/documents/{id}/versions",
                web::get().to(versions::list_versions),
            ),
    )
    .await;

    let request = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{document_id}/versions"))
        .insert_header((
            "Authorization",
            format!("Bearer {}", generate_test_token(user_id, tenant_id)),
        ))
        .to_request();

    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = test::read_body(response).await;

    assert!(
        body.len() < 8 * 1024,
        "a history of five 1 MiB versions must answer in kilobytes, got {} bytes",
        body.len()
    );
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("the response is JSON");
    assert_eq!(parsed["versions"].as_array().map(Vec::len), Some(5));
    assert!(
        parsed["versions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("content").is_none()),
        "the published contract says bodies are not included"
    );
}
