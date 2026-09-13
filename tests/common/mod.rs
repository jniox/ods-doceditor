//! Shared harness for doceditor's integration tests.
//!
//! Why this module exists: every integration test needs the same PostgreSQL
//! connection and the same migration state, and duplicating that per test
//! binary is how the migration table ends up in the wrong schema.
//!
//! Two properties are load-bearing and must not be "simplified" away:
//!
//! 1. `search_path` is pinned to `editor,public` **in the DSN**, because sqlx
//!    creates `_sqlx_migrations` with an unqualified `CREATE TABLE IF NOT
//!    EXISTS` *before* running migration 001. On this shared dev instance,
//!    an unpinned path puts that table in `public`, where it collides with
//!    another service's migration history (symptom: `VersionMissing(7)`).
//! 2. The `editor` schema is created here, before `migrate!` runs, for the
//!    same reason: PostgreSQL silently drops a non-existent schema from
//!    `search_path`, so pinning alone is not enough on a fresh database. It
//!    goes through `repository::schema::ensure_schema_exists` rather than a
//!    bare statement, because the creation races against itself when test
//!    threads start together -- invisible here, fatal on CI's fresh database.
#![allow(dead_code)]

use sqlx::postgres::PgPoolOptions;
use sqlx::Executor;
use uuid::Uuid;

/// Canonical dev DSN, used when `DATABASE_URL` is absent.
///
/// 5435 and not 5433: on this host 5433 is another project's container, which
/// *answers* and then rejects the `ods` role, so the failure reads like broken
/// code. Settled by HR-20260909-035.
const FALLBACK_DSN: &str = "postgres://ods:ods-dev-2026@127.0.0.1:5435/ods\
                            ?options=-c%20search_path%3Deditor%2Cpublic";

pub fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        eprintln!("DATABASE_URL absent — repli sur le DSN de dev canonique {FALLBACK_DSN}");
        FALLBACK_DSN.to_string()
    })
}

/// Connect, ensure the `editor` schema exists, run the (idempotent) migrations.
pub async fn setup_test_pool() -> sqlx::PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("SET search_path = editor, public;")
                    .await
                    .map(|_| ())
            })
        })
        .connect(&database_url())
        .await
        .expect("Failed to connect to test database");

    // Must precede `migrate!`: see the note at the top of this module. Goes
    // through the service's own helper because `CREATE SCHEMA IF NOT EXISTS`
    // races against itself on a fresh database -- which is what CI is, and
    // where three tests died on 2026-09-13. See repository::schema.
    ods_doceditor::repository::schema::ensure_schema_exists(&pool, "editor")
        .await
        .expect("Failed to ensure the editor schema exists");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    pool
}

/// Insert a template row directly (there is no template-authoring API yet).
pub async fn insert_template(
    pool: &sqlx::PgPool,
    tenant_id: Option<Uuid>,
    name: &str,
    content_html: &str,
    created_by: Uuid,
) -> Uuid {
    sqlx::query_scalar(
        r#"INSERT INTO editor.templates (tenant_id, name, content_html, is_system, created_by)
           VALUES ($1, $2, $3, $4, $5) RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(name)
    .bind(content_html)
    .bind(tenant_id.is_none())
    .bind(created_by)
    .fetch_one(pool)
    .await
    .expect("Failed to insert test template")
}
