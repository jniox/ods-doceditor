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

use ods_doceditor::repository::tenant_context::{
    begin_tenant_tx, runtime_role_is_adoptable, session_setup,
};
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

/// The **administrative** pool: `search_path` only, no role adoption.
///
/// Creating the schema, running migrations and seeding a platform-wide template
/// are administrative acts; serving a request is not, and since migration 008
/// the two no longer run as the same role. Anything that needs DDL, or that
/// models an operator rather than a tenant, belongs here.
///
/// It also connects as whatever `DATABASE_URL` says — `ods`, a superuser — which
/// is what makes it the witness of non-vacuity in `tests/rls_enforcement_test.rs`:
/// the same context-free query that returns nothing on [`setup_test_pool`]
/// returns every tenant's rows here.
pub async fn setup_admin_pool() -> sqlx::PgPool {
    let setup = session_setup(false);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .after_connect(move |conn, _meta| {
            let setup = setup.clone();
            Box::pin(async move { conn.execute(setup.as_str()).await.map(|_| ()) })
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

/// The pool the tests exercise the service through — wired **exactly** like the
/// serving pool of `src/main.rs`, runtime role included.
///
/// This is deliberate, and it is the difference between a security control and
/// a claim about one. If the suite ran as the privileged role of the connection
/// string, every repository path would be tested with the policies switched off,
/// and a write path that forgot to open a tenant transaction would stay green
/// here and fail in production the day the role is hardened. The same reasoning
/// as `tests/events_roundtrip.rs`: a guard that is disabled precisely where it
/// was supposed to guard proves nothing.
pub async fn setup_test_pool() -> sqlx::PgPool {
    let admin = setup_admin_pool().await;

    let adopt = runtime_role_is_adoptable(&admin)
        .await
        .expect("the runtime role is checkable");
    let setup = session_setup(adopt);

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .after_connect(move |conn, _meta| {
            let setup = setup.clone();
            Box::pin(async move { conn.execute(setup.as_str()).await.map(|_| ()) })
        })
        .connect(&database_url())
        .await
        .expect("Failed to open the serving test pool");

    admin.close().await;
    pool
}

/// Insert a template row directly (there is no template-authoring API yet).
///
/// Two different acts behind one helper, and the split is the policy's, not a
/// convenience:
///
/// * a **tenant's own** template is something that tenant may write, so it is
///   written under its own tenant context — which exercises the `WITH CHECK`
///   half of the policy rather than stepping around it;
/// * a **platform** template (`tenant_id IS NULL`) is something no tenant may
///   write, by design: migration 007's `WITH CHECK` exists precisely so that no
///   tenant can forge a platform-wide template. Seeding one is an operator act,
///   so the transaction escalates with `SET LOCAL ROLE NONE` — `LOCAL`, so the
///   escalation dies with the transaction and the connection returns to the pool
///   still running as the runtime role.
pub async fn insert_template(
    pool: &sqlx::PgPool,
    tenant_id: Option<Uuid>,
    name: &str,
    content_html: &str,
    created_by: Uuid,
) -> Uuid {
    let mut tx = match tenant_id {
        Some(tenant) => begin_tenant_tx(pool, tenant)
            .await
            .expect("tenant transaction opens"),
        None => {
            let mut tx = pool.begin().await.expect("transaction opens");
            tx.execute("SET LOCAL ROLE NONE")
                .await
                .expect("the fixture can escalate for the length of this transaction");
            tx
        }
    };

    let id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO editor.templates (tenant_id, name, content_html, is_system, created_by)
           VALUES ($1, $2, $3, $4, $5) RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(name)
    .bind(content_html)
    .bind(tenant_id.is_none())
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await
    .expect("Failed to insert test template");

    tx.commit().await.expect("Failed to commit test template");
    id
}

/// Canonical local Redpanda address, used when `REDPANDA_BROKERS` is absent.
///
/// 19092 rather than the conventional 9092: this host runs a dozen services'
/// containers and the round-trip test must not silently attach to somebody
/// else's broker, which would make its assertions depend on another project's
/// retention settings. CI sets `REDPANDA_BROKERS` explicitly and therefore
/// never relies on this constant.
pub const FALLBACK_BROKERS: &str = "127.0.0.1:19092";

/// Where the event round-trip test publishes.
///
/// There is deliberately **no skip path**. A broker-less environment makes the
/// test fail, loudly, rather than pass by inspecting nothing: this service
/// shipped four months of CloudEvents into a `NoopProducer` precisely because
/// nothing ever asserted that a real broker received one. See ADR-002 and the
/// GTM brief's known limitation #2.
pub fn broker_addr() -> String {
    std::env::var("REDPANDA_BROKERS")
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| {
            eprintln!(
                "REDPANDA_BROKERS absent — repli sur le courtier de dev canonique \
                 {FALLBACK_BROKERS} (docker run … redpandadata/redpanda)"
            );
            FALLBACK_BROKERS.to_string()
        })
}
