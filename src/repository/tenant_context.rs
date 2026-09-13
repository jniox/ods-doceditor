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
    pub role: String,
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
    let (role, is_superuser, bypasses_rls): (String, bool, bool) = sqlx::query_as(
        "SELECT rolname, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to read the RLS posture: {e}")))?;

    Ok(RlsPosture {
        role,
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
                "Row-level security is enforced by PostgreSQL for this role"
            );
        }
        Ok(posture) => {
            tracing::warn!(
                role = %posture.role,
                is_superuser = posture.is_superuser,
                bypasses_rls = posture.bypasses_rls,
                "Row-level security is BYPASSED by the database role: tenant \
                 isolation currently rests on the application's tenant_id \
                 predicates alone. Run this service as a non-superuser role \
                 without BYPASSRLS."
            );
        }
        Err(e) => tracing::warn!("Could not determine the RLS posture: {e}"),
    }
}
