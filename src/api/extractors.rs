use actix_web::{FromRequest, HttpRequest, dev::Payload};
use std::future::{Ready, ready};
use uuid::Uuid;

use crate::error::AppError;

/// Authenticated user extracted from JWT claims.
/// In production this validates the JWT; for now we extract from headers
/// to enable testing without a full OID integration.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
}

impl FromRequest for AuthUser {
    type Error = AppError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        // Extract tenant_id from X-Tenant-Id header
        let tenant_id = req
            .headers()
            .get("X-Tenant-Id")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok());

        // Extract user_id from X-User-Id header (simplified; real impl parses JWT sub)
        let user_id = req
            .headers()
            .get("X-User-Id")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok());

        match (tenant_id, user_id) {
            (Some(tid), Some(uid)) => ready(Ok(AuthUser {
                user_id: uid,
                tenant_id: tid,
            })),
            _ => ready(Err(AppError::Unauthorized(
                "Missing or invalid authentication headers".to_string(),
            ))),
        }
    }
}
