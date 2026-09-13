//! AC-011 — tenant isolation must be enforced by PostgreSQL, not only by the
//! `WHERE tenant_id = $n` clauses in the repositories.
//!
//! The BA finding this pins: migration 005 declared `ENABLE ROW LEVEL SECURITY`
//! and stopped there. `ENABLE` does not apply to the table owner, and the role
//! the service connects as on the dev instance carries `BYPASSRLS` on top of
//! that — so the policies were decorative and a single forgotten predicate
//! would have leaked across tenants.

mod common;

use common::setup_test_pool;
use ods_doceditor::repository::tenant_context::rls_posture;

const EDITOR_TABLES: [&str; 3] = ["documents", "document_versions", "templates"];

/// Every table of the schema is both RLS-enabled and RLS-forced.
#[actix_web::test]
async fn test_all_editor_tables_force_row_level_security() {
    let pool = setup_test_pool().await;

    for table in EDITOR_TABLES {
        let (enabled, forced): (bool, bool) = sqlx::query_as(
            r#"SELECT c.relrowsecurity, c.relforcerowsecurity
               FROM pg_class c
               JOIN pg_namespace n ON n.oid = c.relnamespace
               WHERE n.nspname = 'editor' AND c.relname = $1"#,
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("editor.{table} not found: {e}"));

        assert!(enabled, "RLS must be enabled on editor.{table}");
        assert!(
            forced,
            "RLS must be FORCED on editor.{table}: without it the table owner \
             is exempt, which is precisely the role the service connects as"
        );
    }
}

/// A policy that reads the tenant with the strict form of `current_setting`
/// raises `unrecognized configuration parameter` when the context is missing.
/// An error is a worse failure mode than an empty result: it turns a
/// programming mistake into a 500 instead of into "you see nothing".
#[actix_web::test]
async fn test_policies_tolerate_a_missing_tenant_context() {
    let pool = setup_test_pool().await;

    for table in EDITOR_TABLES {
        let qual: String = sqlx::query_scalar(
            "SELECT qual FROM pg_policies WHERE schemaname = 'editor' AND tablename = $1 AND policyname = 'tenant_isolation'",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("no tenant_isolation policy on editor.{table}: {e}"));

        assert!(
            qual.contains("current_setting('app.tenant_id'::text, true)"),
            "policy on editor.{table} must use the missing_ok form of \
             current_setting, found: {qual}"
        );
    }

    // And the write side is constrained too: reading a system template is
    // allowed, forging one for another tenant is not.
    let with_check: Option<String> = sqlx::query_scalar(
        "SELECT with_check FROM pg_policies WHERE schemaname = 'editor' AND tablename = 'documents' AND policyname = 'tenant_isolation'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        with_check
            .as_deref()
            .is_some_and(|c| c.contains("app.tenant_id")),
        "the policy must also constrain writes (WITH CHECK), found: {with_check:?}"
    );
}

/// The service can tell whether the role it connects as actually obeys RLS.
///
/// On the shared dev instance it does not — `ods` is `SUPERUSER` and
/// `BYPASSRLS` — and the point of this test is that the service says so out
/// loud at startup instead of reporting an isolation guarantee it does not have.
#[actix_web::test]
async fn test_rls_posture_reports_a_bypassing_role() {
    let pool = setup_test_pool().await;

    let posture = rls_posture(&pool).await.expect("posture must be readable");

    assert!(!posture.role.is_empty());
    if posture.is_superuser || posture.bypasses_rls {
        assert!(
            !posture.is_enforced(),
            "a superuser or BYPASSRLS role cannot be reported as enforcing RLS"
        );
    } else {
        assert!(posture.is_enforced());
    }
}
