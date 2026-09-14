//! `CREATE SCHEMA IF NOT EXISTS` is not safe against itself.
//!
//! Found on 2026-09-13, by CI, on the first run in this repository's history
//! that ever reached the tests. Three of the twelve `api_test` cases died in
//! the harness:
//!
//!   Failed to ensure the editor schema exists: duplicate key value violates
//!   unique constraint "pg_namespace_nspname_index"  (SQLSTATE 23505)
//!
//! `IF NOT EXISTS` is a check followed by an insert, and the two are not one
//! atomic step. Two connections both look, both find nothing, both insert, and
//! the loser gets a unique violation on the catalog index — an error, not a
//! no-op.
//!
//! It cannot be reproduced on a development machine, which is the whole
//! difficulty: on the shared `ods-postgres` instance the `editor` schema has
//! existed for months, so the existence check short-circuits before any race
//! can happen. It needs a *fresh* database, which is exactly what the CI
//! postgres service is, and what a first deployment is.
//!
//! So this is not a test-harness defect. `src/main.rs` ran the identical
//! statement at startup and would crash-loop two Cloud Run instances starting
//! together against an empty database. Both now go through
//! `ensure_schema_exists`, and this file is what says why.

mod common;

use ods_doceditor::repository::schema::ensure_schema_exists;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use std::sync::Arc;
use tokio::sync::Barrier;
use uuid::Uuid;

/// How many connections race. The CI failure had three colliding out of
/// twelve tests; a barrier and more racers make it reliable rather than
/// occasional.
const RACERS: usize = 12;

async fn pool_with(connections: u32) -> PgPool {
    PgPoolOptions::new()
        .max_connections(connections)
        .connect(&common::database_url())
        .await
        .expect("Failed to connect to the test database")
}

/// A disposable schema name, recognisable as such if a panic ever leaves one
/// behind (BR-0011's convention, applied to a schema).
fn throwaway_schema_name() -> String {
    format!(
        "tmp_doceditor_20260913_schemarace_{}",
        &Uuid::new_v4().simple().to_string()[..8]
    )
}

async fn drop_schema(pool: &PgPool, schema: &str) {
    pool.execute(format!("DROP SCHEMA IF EXISTS {schema} CASCADE").as_str())
        .await
        .expect("Failed to drop the throwaway schema");
}

/// The regression test: concurrent first creation must not fail anyone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_creation_of_a_schema_fails_nobody() {
    let pool = Arc::new(pool_with(RACERS as u32).await);
    let schema = throwaway_schema_name();

    // Fresh, so every racer genuinely attempts the insert. This is the
    // condition a development machine never reproduces.
    drop_schema(&pool, &schema).await;

    // All racers release on the same instant; without this they queue up
    // politely and the second one simply sees the schema already there.
    let gate = Arc::new(Barrier::new(RACERS));

    let mut racers = Vec::with_capacity(RACERS);
    for _ in 0..RACERS {
        let pool = Arc::clone(&pool);
        let gate = Arc::clone(&gate);
        let schema = schema.clone();
        racers.push(tokio::spawn(async move {
            gate.wait().await;
            ensure_schema_exists(&pool, &schema).await
        }));
    }

    let mut failures = Vec::new();
    for racer in racers {
        if let Err(e) = racer.await.expect("a racer task panicked") {
            failures.push(e.to_string());
        }
    }

    drop_schema(&pool, &schema).await;

    assert!(
        failures.is_empty(),
        "{} of {RACERS} concurrent creations of a fresh schema failed, which \
         is how three api_test cases died in CI:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The ordinary path, and the one every developer sees: the schema is already
/// there. Kept explicit so the fix above cannot be mistaken for the whole
/// behaviour.
#[tokio::test]
async fn creating_a_schema_that_already_exists_is_accepted() {
    let pool = pool_with(2).await;
    let schema = throwaway_schema_name();

    ensure_schema_exists(&pool, &schema)
        .await
        .expect("first creation must succeed");
    let second = ensure_schema_exists(&pool, &schema).await;

    drop_schema(&pool, &schema).await;

    assert!(
        second.is_ok(),
        "a schema that already exists must be accepted: {second:?}"
    );
}

/// A schema name reaches the statement by interpolation, since an identifier
/// cannot be a bind parameter. So the identifier is checked rather than
/// trusted.
#[tokio::test]
async fn a_name_that_is_not_a_plain_identifier_is_refused() {
    let pool = pool_with(1).await;

    for hostile in [
        "editor; DROP SCHEMA public CASCADE",
        "editor\"",
        "",
        "1editor",
        "Editor",
    ] {
        assert!(
            ensure_schema_exists(&pool, hostile).await.is_err(),
            "the schema name {hostile:?} must be refused before it reaches SQL"
        );
    }

    // And the name actually used by the service is not caught by the check.
    assert!(ensure_schema_exists(&pool, "editor").await.is_ok());
}
