//! AC-004 / AC-007 / AC-014 — two writers on the same document must each leave
//! a restorable version, and neither may fail.
//!
//! DocEditor is the service several people edit the same document through, so
//! "two saves land at once" is its normal traffic, not an edge case. Both write
//! paths allocated the next version number the same way:
//!
//! ```text
//! SELECT current_version ...      -- plain read, no lock
//! ... compute current_version + 1
//! UPDATE documents SET current_version = <that absolute value>
//! INSERT INTO document_versions (version = <that absolute value>)
//! ```
//!
//! Nothing between the read and the write stops a second transaction from
//! reading the same `current_version`. Both then claim the same version number,
//! and `UNIQUE (document_id, version)` (migration 003) turns the loser into a
//! `23505` — a 500 on a perfectly legitimate request. Without that constraint
//! the same race would have written two rows numbered alike and lost one edit
//! from the history, which is the guarantee ADR-001 exists to give.
//!
//! **How these tests remove the timing luck.** They do not hope the two writers
//! interleave: a third transaction holds the document row with `FOR UPDATE`
//! while both writers start, and is only released once PostgreSQL itself
//! reports (`pg_blocking_pids`) that both are waiting on it. The race is then
//! guaranteed to be attempted, in either direction, on every run.

mod common;

use common::setup_test_pool;
use ods_doceditor::domain::document::DocumentUpdate;
use ods_doceditor::repository::tenant_context::begin_tenant_tx;
use ods_doceditor::repository::{document_repo, version_repo};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

/// Block until `n` backends are queued behind the lock held by `blocker_pid`.
///
/// Asking PostgreSQL rather than sleeping is what makes these tests
/// deterministic: a `sleep(200ms)` would pass on an idle laptop and stop
/// proving anything on a loaded CI runner, in silence.
///
/// The walk is recursive because only the FIRST waiter is blocked by the lock
/// holder; the ones behind it wait on that waiter's *tuple* lock and name it,
/// not the holder, in `pg_blocking_pids`. Counting direct waiters only ever
/// finds one, whatever the contention — measured on this instance while writing
/// these tests.
async fn wait_until_blocked_by(pool: &PgPool, blocker_pid: i32, n: i64) {
    for _ in 0..300 {
        let queued: i64 = sqlx::query_scalar(
            r#"WITH RECURSIVE queued(pid) AS (
                   SELECT pid FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))
                 UNION
                   SELECT a.pid FROM pg_stat_activity a
                     JOIN queued q ON q.pid = ANY(pg_blocking_pids(a.pid))
               )
               SELECT count(*) FROM queued"#,
        )
        .bind(blocker_pid)
        .fetch_one(pool)
        .await
        .expect("pg_stat_activity is readable");
        if queued >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "no {n} backends ever queued behind pid {blocker_pid}: the writers never reached \
         the document row, so this test would prove nothing"
    );
}

/// Open the transaction that pins both writers at their first write, and return
/// it with the backend pid holding the lock.
async fn hold_document_row(
    pool: &PgPool,
    tenant_id: Uuid,
    document_id: Uuid,
) -> (sqlx::Transaction<'_, sqlx::Postgres>, i32) {
    let mut tx = begin_tenant_tx(pool, tenant_id)
        .await
        .expect("blocking transaction opens");

    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .expect("backend pid is readable");

    sqlx::query("SELECT id FROM editor.documents WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
        .bind(document_id)
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await
        .expect("the document row is lockable");

    (tx, pid)
}

/// Version numbers of a document's history, ascending.
async fn version_numbers(pool: &PgPool, tenant_id: Uuid, document_id: Uuid) -> Vec<i32> {
    let mut v: Vec<i32> = version_repo::list_versions(pool, tenant_id, document_id)
        .await
        .expect("history is readable")
        .into_iter()
        .map(|version| version.version)
        .collect();
    v.sort_unstable();
    v
}

/// Two concurrent content updates: both must succeed, and the history must hold
/// both bodies under two distinct version numbers.
#[actix_web::test]
async fn two_concurrent_content_updates_each_leave_a_restorable_version() {
    let pool = setup_test_pool().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let doc = document_repo::create_document(
        &pool,
        tenant_id,
        "Contended document",
        user_id,
        serde_json::json!({}),
        "<p>version one</p>",
    )
    .await
    .expect("document is created");

    let (blocker, blocker_pid) = hold_document_row(&pool, tenant_id, doc.id).await;

    let write_a = document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            title: None,
            status: None,
            metadata: None,
            content: Some("<p>edited by A</p>"),
        },
    );
    let write_b = document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            title: None,
            status: None,
            metadata: None,
            content: Some("<p>edited by B</p>"),
        },
    );
    let release = async {
        wait_until_blocked_by(&pool, blocker_pid, 2).await;
        blocker.rollback().await.expect("blocking transaction ends");
    };

    let (a, b, ()) = tokio::join!(write_a, write_b, release);

    a.expect("concurrent update A must not fail");
    b.expect("concurrent update B must not fail");

    assert_eq!(
        version_numbers(&pool, tenant_id, doc.id).await,
        vec![1, 2, 3],
        "each content mutation must take the next version number, without gap or collision"
    );

    let bodies: Vec<String> = version_repo::list_versions(&pool, tenant_id, doc.id)
        .await
        .expect("history is readable")
        .into_iter()
        .map(|v| v.content)
        .collect();
    for expected in [
        "<p>version one</p>",
        "<p>edited by A</p>",
        "<p>edited by B</p>",
    ] {
        assert!(
            bodies.iter().any(|b| b == expected),
            "{expected:?} is missing from the history {bodies:?}: an edit was lost"
        );
    }

    let doc = document_repo::get_document(&pool, tenant_id, doc.id)
        .await
        .expect("document is readable");
    assert_eq!(
        doc.current_version, 3,
        "the document must point at the last version written"
    );
    assert_eq!(doc.content, bodies_last_writer(&bodies));
}

/// The surviving body is whichever writer committed last; the test only needs
/// it to be one of the two, never the pre-edit body.
fn bodies_last_writer(bodies: &[String]) -> String {
    bodies
        .iter()
        .find(|b| b.as_str() == "<p>edited by A</p>" || b.as_str() == "<p>edited by B</p>")
        .cloned()
        .unwrap_or_default()
}

/// An explicit snapshot (`POST /versions`) racing a content update allocates
/// version numbers through the same read-then-write, and must not collide
/// either — this is the mixed pairing a product actually produces: someone
/// saves while someone else pins a checkpoint.
#[actix_web::test]
async fn an_explicit_snapshot_racing_a_content_update_does_not_collide() {
    let pool = setup_test_pool().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let doc = document_repo::create_document(
        &pool,
        tenant_id,
        "Snapshot race",
        user_id,
        serde_json::json!({}),
        "<p>version one</p>",
    )
    .await
    .expect("document is created");

    let (blocker, blocker_pid) = hold_document_row(&pool, tenant_id, doc.id).await;

    let update = document_repo::update_document(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            title: None,
            status: None,
            metadata: None,
            content: Some("<p>edited while a snapshot was taken</p>"),
        },
    );
    let snapshot = version_repo::create_version(
        &pool,
        tenant_id,
        doc.id,
        user_id,
        Some("checkpoint before review"),
        false,
    );
    let release = async {
        wait_until_blocked_by(&pool, blocker_pid, 2).await;
        blocker.rollback().await.expect("blocking transaction ends");
    };

    let (updated, snapshotted, ()) = tokio::join!(update, snapshot, release);

    updated.expect("the content update must not fail");
    snapshotted.expect("the explicit snapshot must not fail");

    assert_eq!(
        version_numbers(&pool, tenant_id, doc.id).await,
        vec![1, 2, 3],
        "the snapshot and the update must take two distinct, consecutive versions"
    );

    let doc = document_repo::get_document(&pool, tenant_id, doc.id)
        .await
        .expect("document is readable");
    assert_eq!(
        doc.current_version, 3,
        "the document must point at the last version written"
    );
}
