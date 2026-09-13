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

/// The tracing filter to apply at startup.
///
/// `RUST_LOG` wins when it is set, because that is the knob an operator reaches
/// for and overriding it would be surprising. `LOG_LEVEL` is the documented
/// variable of this service and was, until now, parsed into `AppConfig` and
/// then never used — setting it in Cloud Run had no effect whatsoever.
pub fn log_filter(log_level: &str, rust_log: Option<&str>) -> String {
    let non_empty = |v: &str| {
        let v = v.trim();
        (!v.is_empty()).then(|| v.to_string())
    };

    rust_log
        .and_then(non_empty)
        .or_else(|| non_empty(log_level))
        .unwrap_or_else(|| "info".to_string())
}

impl AppConfig {
    /// The filter this configuration asks for, honouring `RUST_LOG` first.
    pub fn log_filter(&self) -> String {
        log_filter(&self.log_level, std::env::var("RUST_LOG").ok().as_deref())
    }

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

#[cfg(test)]
mod tests {
    use super::log_filter;

    #[test]
    fn rust_log_wins_when_an_operator_sets_it() {
        assert_eq!(log_filter("warn", Some("debug")), "debug");
        assert_eq!(
            log_filter("warn", Some("ods_doceditor=trace")),
            "ods_doceditor=trace"
        );
    }

    #[test]
    fn log_level_is_used_when_rust_log_is_absent_or_blank() {
        assert_eq!(log_filter("warn", None), "warn");
        assert_eq!(log_filter("debug", Some("   ")), "debug");
    }

    #[test]
    fn the_fallback_is_info_and_never_an_empty_filter() {
        assert_eq!(log_filter("", None), "info");
        assert_eq!(log_filter("  ", Some("")), "info");
    }
}
