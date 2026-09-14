use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Begin a transaction with tenant context set via SET LOCAL app.tenant_id.
/// This activates PostgreSQL Row-Level Security (RLS) policies for the duration
/// of the transaction.
pub async fn begin_tenant_tx(
    pool: &PgPool,
    tenant_id: Uuid,
) -> AppResult<sqlx::Transaction<'_, sqlx::Postgres>> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to begin transaction: {e}")))?;

    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to set tenant context: {e}")))?;

    Ok(tx)
}

/// The schema this service owns. Every table lives in it, and every pooled
/// connection resolves unqualified names through it.
pub const DB_SCHEMA: &str = "editor";

/// The unprivileged role the serving pool runs every request query as.
///
/// # Why the service drops its own privileges
///
/// Seven BA cycles (2026-09-10 → 2026-09-14) filed the same MEDIUM deviation
/// and closed it the same way: *"remaining fix is an operational role change
/// (non-superuser, non-BYPASSRLS), not code"*. The measurement behind it was
/// correct and re-taken every time — on this host `DATABASE_URL` resolves to
/// `ods`, which is `rolsuper = t, rolbypassrls = t`, and PostgreSQL exempts
/// such a role from every policy no matter what migration 007 marks `FORCE`.
/// Nothing moved in seven cycles, because a repository cannot rotate a secret.
///
/// It does not have to. **PostgreSQL evaluates policies against the *effective*
/// role (`current_user`), not against the role that authenticated.** A superuser
/// session that runs `SET ROLE editor_app` is subject to every policy for the
/// rest of that session. Measured on this instance (PostgreSQL 17) before this
/// code was written, on tables owned by the superuser and marked `FORCE`:
///
/// | session | `current_user` | `SELECT count(*) FROM editor.documents` |
/// |---|---|---|
/// | as connected, no tenant context | `ods` | 1278 — policies never evaluated |
/// | after `SET ROLE`, no tenant context | `editor_app` | **0** — fails closed |
/// | after `SET ROLE` + `app.tenant_id` | `editor_app` | 5 — that tenant only |
/// | as connected, *same* tenant context | `ods` | 1302 across **964** tenants |
///
/// The last line is the one that matters: the tenant context was set in both
/// cases. What changes the answer is which role asks.
///
/// So the service provisions the role itself (migration 008) and adopts it on
/// every connection of the serving pool. No secret to rotate, no operator step,
/// and the same posture on a laptop, on CI, on staging and in production.
///
/// Connecting *directly* as a non-privileged role remains the stronger
/// configuration — it removes the privileges from the connection string, so
/// nothing can `RESET ROLE` back to them. This makes that an improvement rather
/// than a prerequisite.
pub const RUNTIME_ROLE: &str = "editor_app";

/// What an operator would still gain by going further, logged verbatim next to
/// any failing measurement so the remedy never has to be reconstructed.
pub const REMEDIATION: &str =
    "point DATABASE_URL at a plain LOGIN role that is neither SUPERUSER nor BYPASSRLS, \
     and grant it USAGE on the editor schema";

/// The SQL run on every connection the pool hands out.
///
/// `search_path` first, `SET ROLE` second, and never the other way round: the
/// runtime role must not be the one resolving the schema, or a connection could
/// be refused at check-out on a database where it cannot see `editor`.
pub fn session_setup(adopt_runtime_role: bool) -> String {
    if adopt_runtime_role {
        format!("SET search_path = {DB_SCHEMA}, public; SET ROLE {RUNTIME_ROLE};")
    } else {
        format!("SET search_path = {DB_SCHEMA}, public;")
    }
}

/// Can this connection drop into [`RUNTIME_ROLE`], and would that change anything?
///
/// Two conditions, and the second is *tried* rather than inferred:
///
/// 1. the role exists and is itself neither `rolsuper` nor `rolbypassrls` —
///    adopting an exempt role would move `current_user` and protect nothing.
///    Read from `pg_roles`, so a role that acquired either attribute out of band
///    is refused rather than trusted;
/// 2. `SET ROLE` actually succeeds. Membership is not enough: on PostgreSQL 16+
///    a role created by a `CREATEROLE` user is granted back to its creator with
///    `ADMIN` but without `SET`, so `pg_has_role(…, 'MEMBER')` answers true while
///    the statement still fails — and the whole pool would then fail to open a
///    single connection. It is probed once, at startup, on a real connection.
///
/// Returns `false` rather than an error when the role is unusable: a database
/// whose administrative role lacks `CREATEROLE` (a locked-down Neon or Cloud SQL
/// project) must still boot and serve. [`log_rls_posture`] then says so out loud.
pub async fn runtime_role_is_adoptable(pool: &PgPool) -> AppResult<bool> {
    let usable: bool = sqlx::query_scalar(
        "SELECT EXISTS ( \
             SELECT 1 FROM pg_roles r \
              WHERE r.rolname = $1 AND NOT r.rolsuper AND NOT r.rolbypassrls \
         )",
    )
    .bind(RUNTIME_ROLE)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to look up the runtime role: {e}")))?;

    if !usable {
        return Ok(false);
    }

    // One simple-query batch, so PostgreSQL wraps it in an implicit transaction:
    // if `RESET ROLE` were somehow not reached, the `SET ROLE` is rolled back
    // with it and the connection returns to the pool exactly as it left.
    match sqlx::raw_sql(&format!("SET ROLE \"{RUNTIME_ROLE}\"; RESET ROLE;"))
        .execute(pool)
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => {
            tracing::warn!(
                role = RUNTIME_ROLE,
                error = %e,
                "The runtime role exists but this connection cannot adopt it"
            );
            Ok(false)
        }
    }
}

/// What the connected database role can actually be held to.
///
/// `FORCE ROW LEVEL SECURITY` removes the table owner's exemption, but nothing
/// in SQL removes the exemption of a `SUPERUSER` or `BYPASSRLS` role: for such
/// a role the policies are decorative and tenant isolation rests entirely on
/// the `WHERE tenant_id = $n` predicates in the repositories. The service must
/// be able to say which of the two situations it is in rather than advertise a
/// guarantee it does not have.
#[derive(Debug, Clone)]
pub struct RlsPosture {
    /// The **effective** role (`current_user`) — the one PostgreSQL evaluates
    /// the policies against, which is [`RUNTIME_ROLE`] once the pool has
    /// adopted it. This is the field that decides enforcement.
    pub role: String,
    /// The role the connection string authenticated as (`session_user`). Kept
    /// distinct so the logs read "authenticated as ods, running as editor_app"
    /// instead of hiding the privileged half of the truth.
    pub session_role: String,
    pub is_superuser: bool,
    pub bypasses_rls: bool,
}

impl RlsPosture {
    /// True when the database itself will enforce the policies.
    pub fn is_enforced(&self) -> bool {
        !self.is_superuser && !self.bypasses_rls
    }
}

/// Read the posture of the role the pool is connected as.
pub async fn rls_posture(pool: &PgPool) -> AppResult<RlsPosture> {
    let (role, session_role, is_superuser, bypasses_rls): (String, String, bool, bool) =
        sqlx::query_as(
            "SELECT current_user::text, \
                    session_user::text, \
                    coalesce(r.rolsuper, false), \
                    coalesce(r.rolbypassrls, false) \
             FROM (SELECT 1) _ \
             LEFT JOIN pg_roles r ON r.rolname = current_user",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read the RLS posture: {e}")))?;

    Ok(RlsPosture {
        role,
        session_role,
        is_superuser,
        bypasses_rls,
    })
}

/// Log the posture once, at startup, at a level proportional to the risk.
pub async fn log_rls_posture(pool: &PgPool) {
    match rls_posture(pool).await {
        Ok(posture) if posture.is_enforced() => {
            tracing::info!(
                role = %posture.role,
                session_role = %posture.session_role,
                "Row-level security is enforced by PostgreSQL for this role"
            );
        }
        Ok(posture) => {
            tracing::warn!(
                role = %posture.role,
                session_role = %posture.session_role,
                is_superuser = posture.is_superuser,
                bypasses_rls = posture.bypasses_rls,
                remediation = REMEDIATION,
                "Row-level security is BYPASSED by the effective database role: \
                 tenant isolation currently rests on the application's tenant_id \
                 predicates alone. The serving pool could not adopt the \
                 unprivileged runtime role of migration 008."
            );
        }
        Err(e) => tracing::warn!("Could not determine the RLS posture: {e}"),
    }
}
