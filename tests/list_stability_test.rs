//! Walking the pages of a list must enumerate each document exactly once.
//!
//! `GET /api/v1/documents` ordered by `updated_at DESC` alone, and that is not
//! a total order: two documents written by one transaction — a seed, an import,
//! a data migration — carry the same `now()` to the microsecond. PostgreSQL is
//! then free to return tied rows in whatever order the plan it chose produces,
//! and **it does not choose the same plan for every page**. Measured on
//! 2026-09-14 against this schema, 2 000 tied documents:
//!
//! ```text
//! EXPLAIN … LIMIT 100 OFFSET 0    -> Index Scan using idx_documents_tenant_updated
//! EXPLAIN … LIMIT 100 OFFSET 1900 -> Sort  (Sort Key: updated_at DESC)
//!
//! walking all 20 pages:  rows returned 2000 | distinct documents 1999
//!                        tie-00410 returned twice | tie-00801 never returned
//! ```
//!
//! One document is lost and one is duplicated, in a response that reports
//! nothing unusual — the same family as the page this endpoint used to say it
//! had served while serving another. The cure is the same too: make the
//! property true by construction. `ORDER BY updated_at DESC, id DESC` is a
//! total order, because `id` is the primary key.
//!
//! How this is asked without 2 000 rows and without waiting for the planner to
//! change its mind on its own: two pools, one that may only use the index and
//! one that may only scan and sort — the two plans the planner actually picked
//! above — and the walk takes its pages from them alternately. That is a
//! deterministic stand-in for a plan switch mid-list, and it is the technique
//! `tests/search_index_test.rs` already uses to reach the plan an index hides.
//!
//! The first test is the one that fails first, and it is the invariant
//! itself: the same page, asked of the two plans, must come back identical.
//! The second is the consequence a client suffers, and it is deliberately kept
//! even though at forty documents the two plans only disagree *within* a page:
//! the loss appears where the pages' boundaries stop lining up, which is the
//! twenty-page transcript above.

mod common;

use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::domain::pagination::Pagination;
use ods_doceditor::repository::document_repo;
use ods_doceditor::repository::tenant_context::{runtime_role_is_adoptable, session_setup};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use std::collections::BTreeSet;
use uuid::Uuid;

/// Enough documents for several pages; small enough to stay a unit of seconds.
const TIED_DOCUMENTS: i64 = 40;
const PER_PAGE: i64 = 10;

/// The two plans, in the order the planner itself produced them.
const INDEX_ONLY: &str = "SET enable_seqscan = off;";
const SCAN_AND_SORT: &str =
    "SET enable_indexscan = off; SET enable_bitmapscan = off; SET enable_indexonlyscan = off;";

/// The invariant: a page must not depend on the plan that served it.
///
/// This is what "total order" means operationally, and it is the assertion
/// that fails first. Measured on this fixture before the tie-break, page 1
/// came back as `…-0022, …-0021, …, …-0013` from the index and
/// `…-0021, …, …-0013, …-0022` from the sort — the same ten documents in two
/// orders, which is already a broken promise, and the seed of the loss the
/// walk below suffers at scale.
#[tokio::test]
async fn a_page_does_not_depend_on_the_plan_that_served_it() {
    let tenant = Uuid::new_v4();
    seed_tied_documents(tenant).await;

    let index_only = planner_pool(INDEX_ONLY).await;
    let scan_and_sort = planner_pool(SCAN_AND_SORT).await;

    let pages = TIED_DOCUMENTS / PER_PAGE;
    for page in 1..=pages {
        let by_index = page_titles(&index_only, tenant, page).await;
        let by_sort = page_titles(&scan_and_sort, tenant, page).await;

        assert_eq!(
            by_index.len(),
            PER_PAGE as usize,
            "page {page} came back short, so nothing is being compared"
        );
        assert_eq!(
            by_index, by_sort,
            "page {page} of {pages} depends on the plan that served it.\n\
             index scan: {by_index:?}\nscan + sort: {by_sort:?}\n\
             `updated_at` alone is not a total order, and a caller cannot see \
             which plan answered."
        );
    }
}

/// The consequence a client actually suffers: pages that do not partition.
///
/// The planner changes its mind on its own as the offset grows — measured at
/// 2 000 tied documents, `OFFSET 0` ran an index scan and `OFFSET 1900` a sort
/// — so the walk takes its pages from the two plans alternately rather than
/// waiting for a table large enough to provoke the switch.
#[tokio::test]
async fn a_walk_across_a_plan_change_returns_every_document_once() {
    let tenant = Uuid::new_v4();
    let expected = seed_tied_documents(tenant).await;

    let index_only = planner_pool(INDEX_ONLY).await;
    let scan_and_sort = planner_pool(SCAN_AND_SORT).await;

    let pages = TIED_DOCUMENTS / PER_PAGE;
    let mut seen: Vec<String> = Vec::new();
    for page in 1..=pages {
        let pool = if page % 2 == 1 {
            &index_only
        } else {
            &scan_and_sort
        };
        seen.extend(page_titles(pool, tenant, page).await);
    }

    let distinct: BTreeSet<String> = seen.iter().cloned().collect();
    let mut duplicated: Vec<&String> = seen.iter().filter(|t| count(&seen, t) > 1).collect();
    duplicated.sort();
    duplicated.dedup();
    let missing: Vec<&String> = expected.iter().filter(|t| !distinct.contains(*t)).collect();

    assert!(
        duplicated.is_empty() && missing.is_empty(),
        "walking {pages} pages of {TIED_DOCUMENTS} documents returned {} rows \
         holding {} distinct documents.\nreturned twice: {duplicated:?}\nnever \
         returned: {missing:?}\nThe pages do not partition the collection.",
        seen.len(),
        distinct.len()
    );
    assert_eq!(
        distinct.len(),
        TIED_DOCUMENTS as usize,
        "every seeded document must appear exactly once"
    );
}

/// One page, through the service's own repository, on the given plan.
async fn page_titles(pool: &PgPool, tenant: Uuid, page: i64) -> Vec<String> {
    let (documents, total) = document_repo::list_documents(
        pool,
        tenant,
        Pagination::new(Some(page), Some(PER_PAGE)),
        None,
        None,
    )
    .await
    .expect("a page of a tenant's own documents is readable");

    assert_eq!(
        total, TIED_DOCUMENTS,
        "the total must not depend on the plan either"
    );
    documents.into_iter().map(|d| d.title).collect()
}

fn count(haystack: &[String], needle: &str) -> usize {
    haystack.iter().filter(|t| t.as_str() == needle).count()
}

/// Create the documents, then flatten their `updated_at` to a single value.
///
/// The flattening is one `UPDATE`, which is what a bulk write is: `now()` is
/// the *transaction* timestamp, so every row it touches ties exactly. Returns
/// the titles, which stand in for the documents.
async fn seed_tied_documents(tenant: Uuid) -> BTreeSet<String> {
    let pool = common::setup_test_pool().await;
    let author = Uuid::new_v4();
    let mut titles = BTreeSet::new();

    for i in 1..=TIED_DOCUMENTS {
        let title = format!("tied-{i:04}");
        document_repo::create_document(
            &pool,
            tenant,
            &common::title(&title),
            author,
            &Metadata::empty(),
            "",
        )
        .await
        .expect("a document is creatable");
        titles.insert(title);
    }
    pool.close().await;

    // An operator act (see `common::insert_template` for the same reasoning):
    // no API sets `updated_at`, and what is being modelled is a write that did
    // not come through one.
    let admin = common::setup_admin_pool().await;
    sqlx::query("UPDATE editor.documents SET updated_at = now() WHERE tenant_id = $1")
        .bind(tenant)
        .execute(&admin)
        .await
        .expect("the fixture must be able to flatten updated_at");

    let tied: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT updated_at) FROM editor.documents WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_one(&admin)
    .await
    .expect("the fixture is readable");
    admin.close().await;
    assert_eq!(
        tied, 1,
        "the fixture must actually tie every row, or there is nothing to break"
    );

    titles
}

/// A pool wired like the serving one, plus the planner settings that pin it to
/// one plan.
async fn planner_pool(planner: &'static str) -> PgPool {
    let admin = common::setup_admin_pool().await;
    let adopt = runtime_role_is_adoptable(&admin)
        .await
        .expect("the runtime role is checkable");
    admin.close().await;

    let setup = format!("{} {planner}", session_setup(adopt));
    PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |conn, _meta| {
            let setup = setup.clone();
            Box::pin(async move { conn.execute(setup.as_str()).await.map(|_| ()) })
        })
        .connect(&common::database_url())
        .await
        .expect("Failed to open a plan-pinned pool")
}
