use actix_web::{dev::Payload, web, FromRequest, HttpRequest};
use jsonwebtoken::{decode, Algorithm, DecodingKey, TokenData, Validation};
use serde::{Deserialize, Serialize};
use std::future::{ready, Ready};
use uuid::Uuid;

use crate::error::AppError;

/// JWT claims expected in ODS tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub tenant_id: String,
    pub exp: usize,
    pub iat: usize,
    #[serde(default)]
    pub iss: String,
    #[serde(default)]
    pub aud: serde_json::Value,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Configuration for JWT validation, shared as app_data.
#[derive(Clone)]
pub struct JwtConfig {
    pub decoding_key: DecodingKey,
    pub validation: Validation,
}

impl JwtConfig {
    /// Create from base64-encoded RSA public key PEM.
    pub fn from_rsa_pem_b64(
        pem_b64: &str,
        issuer: Option<&str>,
        audience: Option<&str>,
    ) -> Result<Self, String> {
        let pem_bytes = base64_decode(pem_b64)
            .map_err(|e| format!("Failed to decode base64 public key: {e}"))?;

        let decoding_key = DecodingKey::from_rsa_pem(&pem_bytes)
            .map_err(|e| format!("Failed to parse RSA public key: {e}"))?;

        let mut validation = Validation::new(Algorithm::RS256);
        if let Some(iss) = issuer {
            validation.set_issuer(&[iss]);
        }
        if let Some(aud) = audience {
            validation.set_audience(&[aud]);
        }

        Ok(Self {
            decoding_key,
            validation,
        })
    }

    /// Create for HS256 (dev/test only).
    pub fn from_hs256_secret(secret: &str, issuer: Option<&str>, audience: Option<&str>) -> Self {
        let decoding_key = DecodingKey::from_secret(secret.as_bytes());
        let mut validation = Validation::new(Algorithm::HS256);
        if let Some(iss) = issuer {
            validation.set_issuer(&[iss]);
        }
        if let Some(aud) = audience {
            validation.set_audience(&[aud]);
        }
        Self {
            decoding_key,
            validation,
        }
    }

    /// Decode and validate a JWT token.
    pub fn decode_token(
        &self,
        token: &str,
    ) -> Result<TokenData<Claims>, jsonwebtoken::errors::Error> {
        decode::<Claims>(token, &self.decoding_key, &self.validation)
    }
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input.trim())
        .map_err(|e| format!("Base64 decode error: {e}"))
}

/// Authenticated user extracted from JWT Bearer token.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
}

impl FromRequest for AuthUser {
    type Error = AppError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let jwt_config = match req.app_data::<web::Data<JwtConfig>>() {
            Some(config) => config,
            None => {
                tracing::error!("JwtConfig not configured in app_data");
                return ready(Err(AppError::Internal(
                    "Authentication not configured".to_string(),
                )));
            }
        };

        // Extract Bearer token from Authorization header
        let token = match extract_bearer_token(req) {
            Some(t) => t,
            None => {
                return ready(Err(AppError::Unauthorized(
                    "Missing or invalid Authorization header".to_string(),
                )));
            }
        };

        // Validate JWT
        let token_data = match jwt_config.decode_token(token) {
            Ok(data) => data,
            Err(e) => {
                tracing::debug!("JWT validation failed: {e}");
                return ready(Err(AppError::Unauthorized(
                    "Invalid or expired token".to_string(),
                )));
            }
        };

        // Extract user_id (sub) and tenant_id from claims
        let user_id = match Uuid::parse_str(&token_data.claims.sub) {
            Ok(id) => id,
            Err(_) => {
                return ready(Err(AppError::Unauthorized(
                    "Invalid user ID in token".to_string(),
                )));
            }
        };

        let tenant_id = match Uuid::parse_str(&token_data.claims.tenant_id) {
            Ok(id) => id,
            Err(_) => {
                return ready(Err(AppError::Unauthorized(
                    "Invalid tenant ID in token".to_string(),
                )));
            }
        };

        ready(Ok(AuthUser { user_id, tenant_id }))
    }
}

fn extract_bearer_token(req: &HttpRequest) -> Option<&str> {
    req.headers()
        .get("Authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    const TEST_SECRET: &str = "test-secret-for-doceditor-jwt-validation-only";

    /// Create a JwtConfig for tests (HS256).
    pub fn test_jwt_config() -> JwtConfig {
        JwtConfig::from_hs256_secret(TEST_SECRET, None, None)
    }

    /// Generate a valid test JWT token for the given user/tenant.
    pub fn generate_test_token(user_id: Uuid, tenant_id: Uuid) -> String {
        let claims = Claims {
            sub: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            iat: chrono::Utc::now().timestamp() as usize,
            exp: (chrono::Utc::now().timestamp() + 3600) as usize,
            iss: String::new(),
            aud: serde_json::Value::Null,
            roles: vec![],
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(TEST_SECRET.as_bytes()),
        )
        .expect("Failed to generate test token")
    }

    /// Generate an expired test JWT token.
    pub fn generate_expired_token(user_id: Uuid, tenant_id: Uuid) -> String {
        let claims = Claims {
            sub: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            iat: (chrono::Utc::now().timestamp() - 7200) as usize,
            exp: (chrono::Utc::now().timestamp() - 3600) as usize,
            iss: String::new(),
            aud: serde_json::Value::Null,
            roles: vec![],
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(TEST_SECRET.as_bytes()),
        )
        .expect("Failed to generate test token")
    }
}
