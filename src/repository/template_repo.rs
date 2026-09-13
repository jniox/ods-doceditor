use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::template::Template;
use crate::error::{AppError, AppResult};
use crate::repository::tenant_context::begin_tenant_tx;

/// Fetch a template usable by `tenant_id`.
///
/// Visibility rule: a tenant's own templates, plus the platform's system
/// templates (`tenant_id IS NULL AND is_system`). Anything else does not exist
/// as far as the caller is concerned — a template belonging to another tenant
/// must be indistinguishable from an unknown id, or the endpoint becomes an
/// existence oracle across tenants.
pub async fn get_template(
    pool: &PgPool,
    tenant_id: Uuid,
    template_id: Uuid,
) -> AppResult<Template> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;

    let template: Option<Template> = sqlx::query_as(
        r#"SELECT id, tenant_id, name, content_html, is_system
           FROM editor.templates
           WHERE id = $1
             AND (tenant_id = $2 OR (tenant_id IS NULL AND is_system = true))"#,
    )
    .bind(template_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("Commit failed: {e}")))?;

    template.ok_or_else(|| AppError::NotFound("Template not found".to_string()))
}
