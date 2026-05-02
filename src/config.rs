/// Application configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub server_host: String,
    pub server_port: u16,
    pub log_level: String,
    pub redpanda_brokers: Option<String>,
    pub redpanda_topic: String,
    pub max_document_size_mb: usize,
    // JWT config
    pub jwt_rsa_public_key_b64: Option<String>,
    pub jwt_allow_hs256: bool,
    pub jwt_secret: Option<String>,
    pub jwt_issuer: Option<String>,
    pub jwt_audience: Option<String>,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            server_host: std::env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            server_port: std::env::var("SERVER_PORT")
                .unwrap_or_else(|_| "8087".to_string())
                .parse()
                .expect("SERVER_PORT must be a valid u16"),
            log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
            redpanda_brokers: std::env::var("REDPANDA_BROKERS").ok(),
            redpanda_topic: std::env::var("REDPANDA_TOPIC")
                .unwrap_or_else(|_| "editor.events".to_string()),
            max_document_size_mb: std::env::var("MAX_DOCUMENT_SIZE_MB")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10),
            jwt_rsa_public_key_b64: std::env::var("JWT_RSA_PUBLIC_KEY_B64").ok(),
            jwt_allow_hs256: std::env::var("JWT_ALLOW_HS256")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            jwt_secret: std::env::var("JWT_SECRET").ok(),
            jwt_issuer: std::env::var("JWT_ISSUER").ok(),
            jwt_audience: std::env::var("JWT_AUDIENCE").ok(),
        }
    }
}
