/// Bytes in the megabyte `MAX_DOCUMENT_SIZE_MB` counts.
const BYTES_PER_MB: usize = 1024 * 1024;

/// The body ceiling applied when an operator sets none — which is the case in
/// production: `ops/cloudrun/doceditor.json` declares no `MAX_DOCUMENT_SIZE_MB`
/// at all, so **this constant is the deployed ceiling**, not a fallback for
/// local runs.
///
/// 2 MB, and the number is measured rather than chosen. It is one of three
/// settings that only make sense together — the body ceiling, the instance
/// memory (`512Mi`) and the number of requests admitted at once (Cloud Run's
/// default, 80, since that file sets no `--concurrency`). Measured 2026-09-14 on
/// the running binary in a cgroup at 512 MiB, N concurrent full-size saves of
/// distinct documents:
///
/// ```text
/// 10 MB, N=10 -> 200 x10, peak 451 MiB      10 MB, N=12 -> oom-kill, MainPID=0
///  4 MB, N=80 -> oom-kill, MainPID=0         3 MB, N=80 -> 200 x80, peak 475 MiB
///  2 MB, N=80 -> 200 x80, peak 340 MiB
/// ```
///
/// An OOM kill is not a failed request: on Cloud Run the *instance* dies, so
/// every other tenant's in-flight request dies with it. At 10 MB, eleven
/// ordinary saves did that. At 2 MB the platform's own default concurrency fits
/// with a third of the memory to spare.
///
/// Settled by HR-20260914-001 (option A, 2026-09-14, it@orbusdigital.com,
/// "Exécutant : dev, dans le dépôt doceditor"). Raising it again is a sizing
/// decision with the same three terms, not a refactor.
pub const DEFAULT_MAX_DOCUMENT_SIZE_MB: usize = 2;

/// The same ceiling in the unit every enforcement site actually uses.
///
/// `DocumentService` compares it with `str::len()` — **bytes**, not characters,
/// and deliberately so: storage cost is bytes. The published contract says so
/// in the same words, because a bound counted in one unit and published in
/// another is how the largest storable *title* came to depend on the alphabet
/// it was written in (batch 10).
pub const DEFAULT_MAX_DOCUMENT_BYTES: usize = DEFAULT_MAX_DOCUMENT_SIZE_MB * BYTES_PER_MB;

/// Read `MAX_DOCUMENT_SIZE_MB`, refusing a value this service cannot honour
/// instead of quietly serving a different one.
///
/// `.parse().unwrap_or(10)` — what this did until 2026-09-14 — turns
/// `MAX_DOCUMENT_SIZE_MB=2MB`, `=2 mb` or a stray quote into the *default*
/// ceiling, silently. That is the defect this repository has been closing one
/// site at a time: the value that was validated must be the value that applies.
/// It matters more now than it did: since HR-20260914-001 this ceiling is a
/// safety setting, and an operator who lowers it and is not obeyed learns
/// nothing until an instance dies.
///
/// * absent, or present and blank — the documented default;
/// * a positive integer — that number, whatever its size (`max_document_bytes`
///   saturates, so a very large one cannot wrap into a very small one);
/// * anything else, including `0` — an error the caller turns into a refusal to
///   boot, exactly as a malformed `SERVER_PORT` already does. A ceiling of zero
///   is not a policy of storing nothing, it is a typo.
pub fn parse_max_document_size_mb(raw: Option<&str>) -> Result<usize, String> {
    let Some(value) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(DEFAULT_MAX_DOCUMENT_SIZE_MB);
    };
    match value.parse::<usize>() {
        Ok(0) | Err(_) => Err(format!(
            "MAX_DOCUMENT_SIZE_MB must be a positive whole number of megabytes; got {value:?}. \
             Leave it unset for the default of {DEFAULT_MAX_DOCUMENT_SIZE_MB}."
        )),
        Ok(mb) => Ok(mb),
    }
}

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
    /// The body ceiling in bytes — the unit every enforcement site uses.
    ///
    /// Saturating, and that is not decoration: `max_document_size_mb` is an
    /// operator value, and a plain `mb * 1024 * 1024` (what `main.rs` did until
    /// 2026-09-14) wraps a large one into a small one on a release build while
    /// panicking on a debug one. `api::payload::payload_ceiling` already
    /// saturates for precisely this reason and documents it — and was then
    /// handed a number that had already overflowed one line earlier.
    pub fn max_document_bytes(&self) -> usize {
        self.max_document_size_mb.saturating_mul(BYTES_PER_MB)
    }

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
            max_document_size_mb: parse_max_document_size_mb(
                std::env::var("MAX_DOCUMENT_SIZE_MB").ok().as_deref(),
            )
            .unwrap_or_else(|e| panic!("{e}")),
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
    use super::{
        log_filter, parse_max_document_size_mb, AppConfig, DEFAULT_MAX_DOCUMENT_BYTES,
        DEFAULT_MAX_DOCUMENT_SIZE_MB,
    };

    fn config_with(mb: usize) -> AppConfig {
        AppConfig {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 0,
            log_level: String::new(),
            redpanda_brokers: None,
            redpanda_topic: String::new(),
            max_document_size_mb: mb,
            jwt_rsa_public_key_b64: None,
            jwt_allow_hs256: false,
            jwt_secret: None,
            jwt_issuer: None,
            jwt_audience: None,
        }
    }

    /// An operator who sets nothing gets the documented ceiling — which is the
    /// deployed case: `ops/cloudrun/doceditor.json` sets no such variable.
    #[test]
    fn an_unset_ceiling_is_the_documented_default() {
        assert_eq!(
            parse_max_document_size_mb(None),
            Ok(DEFAULT_MAX_DOCUMENT_SIZE_MB)
        );
        assert_eq!(
            parse_max_document_size_mb(Some("   ")),
            Ok(DEFAULT_MAX_DOCUMENT_SIZE_MB)
        );
    }

    /// And one who sets a number gets that number, not a number near it.
    #[test]
    fn the_number_an_operator_writes_is_the_number_that_applies() {
        assert_eq!(parse_max_document_size_mb(Some("5")), Ok(5));
        assert_eq!(parse_max_document_size_mb(Some(" 50 ")), Ok(50));
    }

    /// A value this service cannot honour stops the boot instead of being
    /// silently replaced by a different ceiling.
    ///
    /// `"2MB"` is the shape of the mistake: plausible, rejected by `parse`, and
    /// — until 2026-09-14 — served as 10 MB without a word in the logs. Since
    /// HR-20260914-001 this ceiling is a safety setting, so being ignored is
    /// worse than refusing to start: the instance dies later instead, and the
    /// configuration file still reads as though it had been obeyed.
    #[test]
    fn a_ceiling_this_service_cannot_honour_is_refused_rather_than_replaced() {
        for bad in ["2MB", "abc", "-1", "2.5", "0", "1_000"] {
            let refusal = parse_max_document_size_mb(Some(bad))
                .expect_err("{bad} is not a positive whole number of megabytes");
            assert!(
                refusal.contains("MAX_DOCUMENT_SIZE_MB") && refusal.contains(bad),
                "the refusal must name the variable and the value it rejected; got {refusal:?}"
            );
        }
    }

    /// The bytes the ceiling comes to are computed once, and they saturate.
    ///
    /// `mb * 1024 * 1024` — what `main.rs` did until 2026-09-14, one line
    /// before handing the result to a function that carefully saturates —
    /// panics on a debug build and *wraps* on the release build the Dockerfile
    /// produces, which turns a huge configured ceiling into a tiny effective
    /// one. Same shape as `Pagination::offset`, and the same cure.
    #[test]
    fn the_byte_ceiling_saturates_instead_of_wrapping() {
        assert_eq!(
            config_with(DEFAULT_MAX_DOCUMENT_SIZE_MB).max_document_bytes(),
            DEFAULT_MAX_DOCUMENT_BYTES
        );
        assert_eq!(config_with(usize::MAX).max_document_bytes(), usize::MAX);
        assert!(config_with(usize::MAX / 2).max_document_bytes() >= usize::MAX / 2);
    }

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
