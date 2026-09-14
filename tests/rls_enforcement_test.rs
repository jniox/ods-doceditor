//! AC-011, the half that seven BA cycles kept filing as "operational, not code".
//!
//! Migration 007 marks the three `editor` tables `FORCE ROW LEVEL SECURITY` and
//! gives each a `tenant_isolation` policy with a `WITH CHECK`. `tests/rls_test.rs`
//! proves all of that is *on the tables*. It cannot prove the policies are ever
//! **evaluated**, and until migration 008 they never were: PostgreSQL exempts any
//! role carrying `rolsuper` or `rolbypassrls` from every policy, `FORCE` or not,
//! and `DATABASE_URL` resolves to `ods` — `rolsuper = t, rolbypassrls = t` — on
//! this host, on CI, and on the shared dev instance.
//!
//! The remediation on file was to provision a role and rotate a secret. Seven
//! cycles later nothing had moved, because a repository cannot rotate a secret.
//! It does not have to: **policies are evaluated against the effective role**, so
//! a session that runs `SET ROLE editor_app` is subject to all of them from that
//! point on. Migration 008 provisions the role; the serving pool adopts it on
//! every connection.
//!
//! What this file asserts, therefore, is not "the policy exists" but "a query
//! that forgets its `WHERE tenant_id = $1` returns nothing anyway" — which is the
//! only form of defence-in-depth worth the name.
//!
//! **Non-vacuity is built in, not argued.** `a_privileged_connection_still_sees_
//! every_tenant` runs the *identical* context-free query on the administrative
//! pool and requires it to see several tenants' rows. Without it, an empty table,
//! a wrong schema or a broken DSN would make every assertion below pass while
//! proving nothing at all.

mod common;

use common::{setup_admin_pool, setup_test_pool};
use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::repository::document_repo;
use ods_doceditor::repository::tenant_context::{
    begin_tenant_tx, rls_posture, runtime_role_is_adoptable, session_setup, RUNTIME_ROLE,
};
use sqlx::PgPool;
use uuid::Uuid;

/// The query the whole file turns on: how much of `editor.documents` does this
/// connection see when nobody scoped it? Deliberately free of any `tenant_id`
/// predicate — the application layer is not what is under test here.
async fn documents_visible(pool: &PgPool) -> (i64, i64) {
    sqlx::query_as("SELECT count(*), count(DISTINCT tenant_id) FROM editor.documents")
        .fetch_one(pool)
        .await
        .expect("editor.documents is readable")
}

/// Create one document for `tenant`, through the service's own write path.
async fn seed(pool: &PgPool, tenant: Uuid) -> Uuid {
    document_repo::create_document(
        pool,
        tenant,
        &common::title("RLS enforcement fixture"),
        Uuid::new_v4(),
        &Metadata::empty(),
        "<p>body</p>",
    )
    .await
    .expect("the service can write under its own tenant context")
    .id
}

/// The role exists and carries nothing that would defeat what it is for.
///
/// `NOLOGIN` is part of the contract: the service never authenticates as this
/// role, it drops into it, so the role is one fewer credential to leak or rotate.
#[actix_web::test]
async fn the_runtime_role_exists_and_is_unprivileged() {
    let pool = setup_admin_pool().await;

    let row: Option<(bool, bool, bool)> = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls, rolcanlogin FROM pg_roles WHERE rolname = $1",
    )
    .bind(RUNTIME_ROLE)
    .fetch_optional(&pool)
    .await
    .expect("pg_roles is readable");

    let (is_super, bypasses, can_login) = row.unwrap_or_else(|| {
        panic!(
            "migration 008 must provision the role {RUNTIME_ROLE}: without it every policy on \
             the editor schema is decorative for the role DATABASE_URL connects as"
        )
    });

    assert!(!is_super, "{RUNTIME_ROLE} must not be a superuser");
    assert!(!bypasses, "{RUNTIME_ROLE} must not carry BYPASSRLS");
    assert!(
        !can_login,
        "{RUNTIME_ROLE} must be NOLOGIN: the service adopts it, it never authenticates as it"
    );
}

/// The serving pool runs as the runtime role, not as the connection string's.
#[actix_web::test]
async fn the_serving_pool_drops_into_the_runtime_role() {
    let admin = setup_admin_pool().await;
    assert!(
        runtime_role_is_adoptable(&admin).await.unwrap(),
        "the runtime role must be adoptable after migration 008 — if this fails, read the \
         WARNINGs of that migration: the administrative role is probably missing CREATEROLE"
    );

    let pool = setup_test_pool().await;
    let posture = rls_posture(&pool).await.expect("posture is readable");

    assert_eq!(
        posture.role, RUNTIME_ROLE,
        "every connection of the serving pool must run as {RUNTIME_ROLE}"
    );
    assert!(
        posture.is_enforced(),
        "the effective role must be one PostgreSQL evaluates policies for, found {posture:?}"
    );
}

/// No tenant context, no rows. The safe reading of "nobody said which tenant".
#[actix_web::test]
async fn without_a_tenant_context_the_serving_pool_sees_no_document() {
    let pool = setup_test_pool().await;
    seed(&pool, Uuid::new_v4()).await;

    let (rows, _) = documents_visible(&pool).await;
    assert_eq!(
        rows, 0,
        "a connection that never set app.tenant_id must see nothing; it saw {rows} documents"
    );
}

/// With a tenant context, that tenant's rows and strictly no other's — proven
/// by a query carrying no `tenant_id` predicate of its own.
#[actix_web::test]
async fn with_a_tenant_context_the_database_hides_every_other_tenant() {
    let pool = setup_test_pool().await;

    let mine = Uuid::new_v4();
    let theirs = Uuid::new_v4();
    let my_doc = seed(&pool, mine).await;
    let their_doc = seed(&pool, theirs).await;

    let mut tx = begin_tenant_tx(&pool, mine).await.expect("tenant tx opens");
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM editor.documents")
        .fetch_all(&mut *tx)
        .await
        .expect("editor.documents is readable inside the tenant transaction");
    let tenants: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT tenant_id FROM editor.documents")
        .fetch_all(&mut *tx)
        .await
        .expect("tenant ids are readable");
    tx.commit().await.expect("commit");

    assert_eq!(
        tenants,
        vec![mine],
        "an unscoped SELECT must still only ever reach one tenant's rows"
    );
    assert!(
        ids.contains(&my_doc),
        "the caller's own document must be visible"
    );
    assert!(
        !ids.contains(&their_doc),
        "another tenant's document leaked through an unscoped SELECT"
    );
}

/// The write side too: `WITH CHECK` refuses a row nobody claimed.
#[actix_web::test]
async fn without_a_tenant_context_a_write_is_refused() {
    let pool = setup_test_pool().await;

    let err = sqlx::query(
        r#"INSERT INTO editor.documents (tenant_id, title, status, created_by, metadata, content, word_count)
           VALUES ($1, 'forged', 'draft', $2, '{}'::jsonb, '', 0)"#,
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .expect_err("an insert with no tenant context must be refused by the policy");

    let message = err.to_string();
    assert!(
        message.contains("row-level security"),
        "the refusal must come from the policy, not from something else: {message}"
    );
}

/// **The non-vacuity witness.** The same context-free query, on the same tables,
/// over the same data, on the administrative connection — which is what every
/// test in this repository used to run as. It must see several tenants.
///
/// If this one ever goes green-by-emptiness the four above mean nothing, so it is
/// the first thing to read when they all pass suspiciously fast.
#[actix_web::test]
async fn a_privileged_connection_still_sees_every_tenant() {
    let serving = setup_test_pool().await;
    seed(&serving, Uuid::new_v4()).await;
    seed(&serving, Uuid::new_v4()).await;

    let admin = setup_admin_pool().await;
    let (rows, tenants) = documents_visible(&admin).await;

    assert!(
        rows >= 2 && tenants >= 2,
        "the privileged pool must still see across tenants ({rows} rows, {tenants} tenants) — \
         otherwise the assertions of this file are satisfied by an empty table, not by a policy"
    );
}

/// `search_path` before `SET ROLE`, never the reverse: the runtime role must not
/// be the one resolving the schema, or a pool could fail to check out a single
/// connection on a database where that role cannot see `editor`.
#[test]
fn the_session_setup_resolves_the_schema_before_dropping_privileges() {
    let sql = session_setup(true);
    let path = sql.find("search_path").expect("search_path is set");
    let role = sql.find("SET ROLE").expect("the role is adopted");
    assert!(path < role, "search_path must come first: {sql}");
    assert!(sql.contains(RUNTIME_ROLE), "{sql}");

    let without = session_setup(false);
    assert!(
        !without.contains("SET ROLE"),
        "the administrative wiring must not adopt the role: {without}"
    );
    assert!(without.contains("search_path"), "{without}");
}
