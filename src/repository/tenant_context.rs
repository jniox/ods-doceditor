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
