//! The body this service accepts, and the search index that could not hold it.
//!
//! `MAX_DOCUMENT_SIZE_MB` (10 by default) is published — in `.env.example`, in
//! the repo's `CLAUDE.md` and in `docs/openapi.yaml` — as the largest body
//! DocEditor stores, and `api::payload` exists precisely so that a body of
//! exactly that size can be carried over HTTP. Underneath, every document was
//! also written into a GIN index over
//! `to_tsvector('english', title || ' ' || content)` (migration 006), and a
//! PostgreSQL `tsvector` cannot exceed **1 048 575 bytes of lexemes**. An index
//! expression that raises makes the *write* raise: the row cannot be stored at
//! all.
//!
//! What blows that budget is the **vocabulary** of the text, not its length, so
//! the real ceiling was a number no caller could compute and none was ever
//! told. Measured directly on this PostgreSQL 17 instance before this file
//! existed:
//!
//! ```text
//! body 348 893 B, every word distinct     -> indexable
//! body 708 893 B, every word distinct     -> indexable
//! body 798 893 B, every word distinct     -> ERROR: string is too long for tsvector
//! body 1 888 894 B, every word distinct   -> ERROR (2 598 012 bytes of lexemes)
//! body 10 050 000 B, repetitive prose     -> indexable
//! ```
//!
//! So a 9.6 MiB contract of ordinary prose stored fine while a 0.8 MB annex of
//! reference codes answered `500 {"error":"internal_error"}` — on a `POST` that
//! broke none of the published rules, on the service whose entire purpose is to
//! own the document body. Same family as the two ceilings of
//! `tests/size_limit_test.rs` and the byte/character bound of
//! `tests/text_bounds_test.rs`: **a limit whose real value depends on which
//! characters the caller happened to use.**
//!
//! The fix bounds what is *indexed*, never what is *stored*: migration 009
//! names the searchable projection once — `editor.searchable_text(title,
//! content)`, the first [`INDEXED_PREFIX_CHARS`] characters of the title
//! followed by the body — and both the index and this service's own predicate
//! go through it. Two properties are load-bearing and each has a test below:
//!
//! 1. the **write** must no longer depend on the vocabulary of the body;
//! 2. the **read** must use the same bounded expression. Truncating only the
//!    index would move the failure rather than remove it — a sequential scan
//!    evaluates the predicate row by row, so an untruncated predicate would
//!    raise the very same error on `GET /api/v1/documents?search=…` for every
//!    tenant that owns one large document. The fourth test forces exactly that
//!    plan instead of hoping the planner avoids it.

mod common;

use actix_web::{http::StatusCode, test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, payload};
use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::domain::pagination::Pagination;
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::repository::document_repo::{
    search_predicate, INDEXED_PREFIX_CHARS, SEARCHABLE_TEXT,
};
use ods_doceditor::repository::tenant_context::{runtime_role_is_adoptable, session_setup};
use ods_doceditor::service::document_service::DocumentService;
use sqlx::postgres::PgPoolOptions;
use sqlx::Executor;
use std::sync::Arc;
use uuid::Uuid;

/// The documented default, so these assertions are about the ceiling this
/// service actually publishes rather than about a convenient small one.
const BODY_CEILING: usize = 10 * 1024 * 1024;

/// A body every word of which is distinct — an annex of reference codes, a
/// pasted CSV export, a generated list of identifiers. ~1.05 MB, comfortably
/// inside the published 10 MB ceiling and comfortably past the ~0.76 MB at
/// which the untruncated index gave up.
fn body_of_distinct_words(count: usize) -> String {
    let mut body = String::with_capacity(count * 9);
    for n in 0..count {
        if n > 0 {
            body.push(' ');
        }
        body.push_str("ref");
        body.push_str(&n.to_string());
    }
    body
}

macro_rules! app_with_limits {
    ($pool:expr, $svc:expr, $jwt:expr) => {{
        test::init_service(
            App::new()
                .app_data(web::Data::new($pool.clone()))
                .app_data(web::Data::new($svc.clone()))
                .app_data(web::Data::new($jwt))
                // The same call `main.rs` makes.
                .configure(payload::limits(BODY_CEILING))
                .route(
                    "/api/v1/documents",
                    web::post().to(documents::create_document),
                )
                .route(
                    "/api/v1/documents",
                    web::get().to(documents::list_documents),
                )
                .route(
                    "/api/v1/documents/{id}",
                    web::get().to(documents::get_document),
                ),
        )
        .await
    }};
}

/// A body within the documented ceiling is stored, whatever its vocabulary.
///
/// This is the defect, stated as the product's own promise: a caller who reads
/// `MAX_DOCUMENT_SIZE_MB=10` and sends one megabyte gets a document, not a
/// `500`.
#[actix_web::test]
async fn a_body_of_entirely_distinct_words_is_stored_like_any_other() {
    let pool = setup_test_pool().await;
    let svc = DocumentService::new(pool.clone(), Arc::new(InMemoryProducer::new()))
        .with_max_content_bytes(BODY_CEILING);
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app_with_limits!(pool, svc, test_jwt_config());

    let content = body_of_distinct_words(120_000);
    assert!(
        content.len() < BODY_CEILING,
        "the probe must stay inside the documented body ceiling"
    );

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "Annexe des references",
            "content": content,
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "a {} byte body of distinct words is inside the published ceiling of {BODY_CEILING} \
         bytes; refusing it is a defect, and refusing it with a 500 is the one this test was \
         written for",
        content.len()
    );

    // And it is stored whole: the bound is on what gets indexed, never on what
    // gets kept.
    let created: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/documents")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "title": "Annexe bis", "content": content }))
            .to_request(),
    )
    .await;
    let id = created["id"].as_str().expect("the document has an id");
    let fetched: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri(&format!("/api/v1/documents/{id}"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;
    assert_eq!(
        fetched["content"].as_str().map(str::len),
        Some(content.len()),
        "the whole body comes back — truncation belongs to the index, not to the document"
    );
}

/// Owning such a document must not break the tenant's search.
///
/// The write and the read are two chances to raise the same error, and fixing
/// one without the other only moves it: before the index was bounded such a row
/// could not exist, so the search path had never been asked the question.
#[actix_web::test]
async fn a_tenant_that_owns_a_large_document_can_still_search_its_others() {
    let pool = setup_test_pool().await;
    let svc = DocumentService::new(pool.clone(), Arc::new(InMemoryProducer::new()))
        .with_max_content_bytes(BODY_CEILING);
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app_with_limits!(pool, svc, test_jwt_config());

    for (title, content) in [
        ("Annexe des references", body_of_distinct_words(120_000)),
        (
            "Contrat de prestation",
            "prestation de services entre les parties".to_string(),
        ),
    ] {
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/documents")
                .insert_header(("Authorization", format!("Bearer {token}")))
                .set_json(serde_json::json!({ "title": title, "content": content }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED, "seeding {title}");
    }

    let resp = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/documents?search=prestation")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a tenant owning one large document must still be able to search"
    );
    let page: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/documents?search=prestation")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request(),
    )
    .await;
    assert_eq!(page["total"], 1, "and the search must still find things");
}

/// The bargain, stated out loud: search reaches the first
/// [`INDEXED_PREFIX_CHARS`] characters of a document, and the rest is stored
/// but not indexed.
///
/// A test that only proved "search still works" would pass just as well on an
/// index that had quietly stopped covering anything. This one pins both ends:
/// a word inside the prefix is found, a word past it is not, and the document
/// itself is intact either way.
#[actix_web::test]
async fn search_covers_the_indexed_prefix_and_says_so() {
    let pool = setup_test_pool().await;
    let svc = DocumentService::new(pool.clone(), Arc::new(InMemoryProducer::new()))
        .with_max_content_bytes(BODY_CEILING);
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app_with_limits!(pool, svc, test_jwt_config());

    // Repetitive filler: this test is about *where* the index stops, not about
    // vocabulary, so the body must be indexable in full under the old rule too.
    let filler = "clause de confidentialite entre les parties ".repeat(8_000);
    assert!(
        filler.chars().count() > INDEXED_PREFIX_CHARS,
        "the filler has to push the far marker past the indexed prefix"
    );
    let content = format!("zorglubalpha {filler} zorglubomega");

    let resp = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/documents")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "title": "Marqueurs", "content": content }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    macro_rules! total_for {
        ($term:expr) => {{
            let page: serde_json::Value = test::call_and_read_body_json(
                &app,
                test::TestRequest::get()
                    .uri(&format!("/api/v1/documents?search={}", $term))
                    .insert_header(("Authorization", format!("Bearer {token}")))
                    .to_request(),
            )
            .await;
            page["total"].as_i64().expect("total is a number")
        }};
    }

    assert_eq!(
        total_for!("zorglubalpha"),
        1,
        "a word inside the indexed prefix is searchable"
    );
    assert_eq!(
        total_for!("zorglubomega"),
        0,
        "and one past it is not — that is the documented bargain, not an accident: \
         indexing the whole of a 10 MB body is what PostgreSQL refuses"
    );
}

/// The predicate the service sends and the index it was built on are the same
/// expression — asserted where it matters, on a plan that has no index at all.
///
/// Truncating the index alone would leave `GET …?search=` computing
/// `to_tsvector` over the untruncated body whenever PostgreSQL scans rather
/// than probes, which is exactly the error this batch removed from the write
/// path. Forcing the sequential scan is the difference between testing the
/// property and testing today's planner: with a bitmap scan the predicate is
/// only ever evaluated on rows the index already matched, so the bug would be
/// invisible.
#[actix_web::test]
async fn the_read_path_is_bounded_even_when_postgresql_scans_row_by_row() {
    let seeded = setup_test_pool().await;
    let svc = DocumentService::new(seeded.clone(), Arc::new(InMemoryProducer::new()))
        .with_max_content_bytes(BODY_CEILING);
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());

    svc.create_document(
        tenant_id,
        "Annexe des references",
        user_id,
        &Metadata::empty(),
        Some(&body_of_distinct_words(120_000)),
        None,
    )
    .await
    .expect("a body of distinct words is storable");

    // A pool wired like the serving one, plus a planner that may only scan.
    let adopt = runtime_role_is_adoptable(&seeded)
        .await
        .expect("the runtime role is checkable");
    let setup = format!(
        "{} SET enable_bitmapscan = off; SET enable_indexscan = off; \
         SET enable_indexonlyscan = off;",
        session_setup(adopt)
    );
    let scanning = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |conn, _meta| {
            let setup = setup.clone();
            Box::pin(async move { conn.execute(setup.as_str()).await.map(|_| ()) })
        })
        .connect(&common::database_url())
        .await
        .expect("the scanning pool opens");

    let scanning_svc = DocumentService::new(scanning.clone(), Arc::new(InMemoryProducer::new()));
    let listed = scanning_svc
        .list_documents(
            tenant_id,
            Pagination::new(Some(1), Some(10)),
            None,
            Some("prestation"),
        )
        .await;

    assert!(
        listed.is_ok(),
        "with every index turned off the predicate is evaluated on each row, including the \
         large one: it must be the bounded expression the index was built on. Got {:?}",
        listed.err()
    );
    scanning.close().await;
}

/// The bound itself: whatever alphabet fills the prefix, its `tsvector` fits.
///
/// [`INDEXED_PREFIX_CHARS`] counts **characters** (PostgreSQL's `left()` does)
/// while the 1 048 575-byte limit counts **bytes of lexemes**, so the constant
/// is only safe if the worst-case expansion still fits. Measured here rather
/// than reasoned about, because reasoning about exactly this conversion is what
/// produced the previous two defects in this repository.
#[actix_web::test]
async fn the_indexed_prefix_cannot_overflow_a_tsvector_in_any_alphabet() {
    let pool = common::setup_admin_pool().await;

    // Worst case measured on PostgreSQL 17: many *distinct* short tokens, since
    // duplicates merge. Accented tokens cost the most per character.
    for generator in [
        "SELECT string_agg('r' || g::text, ' ') FROM generate_series(1, $1) g",
        "SELECT string_agg('é' || g::text, ' ') FROM generate_series(1, $1) g",
        "SELECT string_agg(chr(19968 + (g % 20000)), ' ') FROM generate_series(1, $1) g",
    ] {
        let sql = format!(
            "SELECT pg_column_size(to_tsvector('english', left(({generator}), $2)))::bigint"
        );
        let bytes: i64 = sqlx::query_scalar(&sql)
            .bind(INDEXED_PREFIX_CHARS as i32)
            .bind(INDEXED_PREFIX_CHARS as i32)
            .fetch_one(&pool)
            .await
            .expect("the prefix of a pathological body must be indexable at all");

        assert!(
            bytes < 1_048_575,
            "a {INDEXED_PREFIX_CHARS}-character prefix produced {bytes} bytes of tsvector, \
             against PostgreSQL's hard limit of 1048575: the constant is too large"
        );
    }
    pool.close().await;
}

/// PostgreSQL itself confirms that the predicate and the index are one
/// expression — by using the index for it.
///
/// The previous test proves the read is *correct* whatever plan is chosen; this
/// one proves it is still *indexed*. They fail for different reasons and both
/// are worth having: a query that silently stopped matching the index would
/// keep answering, and would degrade to reading every document of the tenant.
#[actix_web::test]
async fn the_predicate_the_service_sends_is_the_one_the_index_was_built_on() {
    let pool = common::setup_admin_pool().await;
    let mut tx = pool.begin().await.expect("a transaction opens");
    tx.execute("SET LOCAL enable_seqscan = off")
        .await
        .expect("the planner can be told to prefer indexes");

    // The production string, not a copy of it.
    let sql = format!(
        "EXPLAIN (COSTS OFF) SELECT id FROM editor.documents WHERE {}",
        search_predicate("$1")
    );
    let plan: Vec<String> = sqlx::query_scalar(&sql)
        .bind("prestation")
        .fetch_all(&mut *tx)
        .await
        .expect("the predicate is valid SQL")
        .to_vec();
    let plan = plan.join("\n");

    assert!(
        plan.contains("idx_documents_searchable_fts"),
        "the full-text predicate must still match the index migration 009 builds, or every \
         search reads the tenant's documents one by one. Plan was:\n{plan}"
    );
    drop(tx);
    pool.close().await;
}

/// One definition of the searchable projection, not two.
///
/// The Rust constant and migration 009 have to agree — the index is built from
/// the migration and the predicate from the constant, and a disagreement is
/// silent: the query still answers, it simply stops using the index and starts
/// evaluating the untruncated expression again.
// `#[test]` would resolve to `actix_web::test`, which this file imports as a
// module: the attribute and the module share a name. See lot 8.
#[actix_web::test]
async fn the_bound_in_the_code_is_the_bound_in_the_migration() {
    let migration = std::fs::read_to_string("migrations/009_bound_the_search_index.sql")
        .expect("migration 009 is part of the repository");

    assert!(
        migration.contains(&INDEXED_PREFIX_CHARS.to_string()),
        "migration 009 must truncate at the same {INDEXED_PREFIX_CHARS} characters the \
         predicate documents"
    );
    assert!(
        migration.contains("searchable_text"),
        "the index must be built on the named projection, not on an inlined copy of it"
    );
    assert!(
        search_predicate("$2").contains(SEARCHABLE_TEXT),
        "and the predicate must go through that same projection"
    );
}
