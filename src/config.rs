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

/// The topic this service publishes to when the deployment names none.
///
/// **`editor-events`**, settled by `spec.md` §4.2 in execution of
/// HR-20260913-001 (option A, it@orbusdigital.com, 2026-09-13: "faire écrire
/// une spec.md doceditor, **et y fixer le nom du topic**"). It had four
/// spellings across four sources and only one of them exists as a provisioned
/// resource — measured 2026-09-14 08:28 UTC among the 25 topics of
/// `orbus-ods-staging`:
///
/// ```text
/// editor-events (+ editor-events-dlq)  EXISTS — provisioned for this service
/// editor.events                        does not exist — this default, until now
/// ods.editor.events                    does not exist — GTM brief, 4 times
/// doceditor-events                     does not exist — the {service}-events rule
/// ```
///
/// `editor` is the **domain**: the schema is `editor`, the CloudEvents source is
/// `/editor`, the types are `com.ods.editor.*`. `doceditor` is the name of the
/// deployment, and consumers read the domain.
///
/// Getting this wrong is silent — the producer returns `Ok` and nothing fails,
/// which is how this service published four months of events into a
/// `NoopProducer` (ADR-002). `tests/event_topic_test.rs` is the only guard it
/// has, because the failure mode has no signature.
pub const DEFAULT_EVENT_TOPIC: &str = "editor-events";

/// Which bus this service publishes to, once the environment has been read.
///
/// An enum and not a pile of `Option`s because the three cases are mutually
/// exclusive and each one needs *different* values to be usable: a broker
/// address, or a project id. Holding them separately is what let a deployment
/// carry `EVENT_BUS=pubsub`, `PUBSUB_TOPIC` and `GCP_PROJECT_ID` since May 2026
/// while the code read `REDPANDA_BROKERS` and nothing else — and drop every
/// event without a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventBus {
    /// No bus at all: events are built and thrown away, loudly at startup.
    Disabled,
    Redpanda {
        brokers: String,
        topic: String,
    },
    PubSub {
        project_id: String,
        topic: String,
    },
}

impl EventBus {
    /// The topic name in force, for the startup log. `None` when nothing is
    /// published — the case that must read differently from "published
    /// somewhere", because publishing nowhere is the silent failure.
    pub fn topic(&self) -> Option<&str> {
        match self {
            EventBus::Disabled => None,
            EventBus::Redpanda { topic, .. } | EventBus::PubSub { topic, .. } => Some(topic),
        }
    }
}

/// The raw environment the selection reads, so the selection itself is a pure
/// function and can be tested without touching the process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct EventBusEnv<'a> {
    pub event_bus: Option<&'a str>,
    pub redpanda_brokers: Option<&'a str>,
    pub redpanda_topic: Option<&'a str>,
    pub pubsub_topic: Option<&'a str>,
    pub gcp_project_id: Option<&'a str>,
}

/// Accepted values of `EVENT_BUS`, named in the refusal so an operator does not
/// have to find this file.
const EVENT_BUS_VALUES: &str = "pubsub, redpanda (alias: kafka), none";

/// Choose the transport, and refuse a choice this service cannot honour.
///
/// Three rules, in this order:
///
/// 1. `EVENT_BUS` set — the deployment has decided. A decision that cannot be
///    carried out (`pubsub` without `GCP_PROJECT_ID`, `redpanda` without
///    `REDPANDA_BROKERS`, a word that is neither) **stops the boot**. This is
///    the same stance `parse_max_document_size_mb` takes, and for a stronger
///    reason: a mis-set ceiling eventually kills an instance, a mis-set bus
///    produces nothing at all to notice.
/// 2. `EVENT_BUS` unset — the local-development path this repository has always
///    had: a broker address selects Redpanda, its absence selects nothing.
/// 3. Nothing set — `Disabled`, and the startup WARN says what that costs.
///
/// The topic falls back to [`DEFAULT_EVENT_TOPIC`] on both transports, because
/// `editor-events` is the name of the resource whatever carries it (spec.md
/// §4.2, HR-20260913-001).
pub fn select_event_bus(env: EventBusEnv<'_>) -> Result<EventBus, String> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }

    let topic_or_default = |explicit: Option<&str>, fallback: Option<&str>| {
        set(explicit)
            .or_else(|| set(fallback))
            .unwrap_or(DEFAULT_EVENT_TOPIC)
            .to_string()
    };

    let Some(choice) = set(env.event_bus) else {
        // No explicit choice: the historical behaviour, unchanged.
        return Ok(match set(env.redpanda_brokers) {
            Some(brokers) => EventBus::Redpanda {
                brokers: brokers.to_string(),
                topic: topic_or_default(env.redpanda_topic, None),
            },
            None => EventBus::Disabled,
        });
    };

    match choice.to_ascii_lowercase().as_str() {
        "pubsub" | "pub/sub" | "google-pubsub" => {
            let project_id = set(env.gcp_project_id).ok_or_else(|| {
                "EVENT_BUS=pubsub needs GCP_PROJECT_ID to know which project's topic to publish \
                 to. Set it, or leave EVENT_BUS unset to run without a bus."
                    .to_string()
            })?;
            Ok(EventBus::PubSub {
                project_id: project_id.to_string(),
                topic: topic_or_default(env.pubsub_topic, env.redpanda_topic),
            })
        }
        "redpanda" | "kafka" => {
            let brokers = set(env.redpanda_brokers).ok_or_else(|| {
                "EVENT_BUS=redpanda needs REDPANDA_BROKERS to know which broker to publish to. \
                 Set it, or leave EVENT_BUS unset to run without a bus."
                    .to_string()
            })?;
            Ok(EventBus::Redpanda {
                brokers: brokers.to_string(),
                topic: topic_or_default(env.redpanda_topic, None),
            })
        }
        "none" | "noop" | "off" => Ok(EventBus::Disabled),
        other => Err(format!(
            "EVENT_BUS={other:?} names no transport this service can use. Accepted: \
             {EVENT_BUS_VALUES}."
        )),
    }
}

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

/// The owned counterpart of [`EventBusEnv`], for reading the process
/// environment before borrowing from it.
struct RawEventBusEnv {
    event_bus: Option<String>,
    redpanda_brokers: Option<String>,
    redpanda_topic: Option<String>,
    pubsub_topic: Option<String>,
    gcp_project_id: Option<String>,
}

/// Application configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub server_host: String,
    pub server_port: u16,
    pub log_level: String,
    /// The transport, already selected and validated. Not the raw variables:
    /// holding `Option<String>`s here is exactly what let the deployment's
    /// choice go unread.
    pub event_bus: EventBus,
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
        // Read once into owned values: `EventBusEnv` borrows, and a temporary
        // built inline would not outlive the call.
        let event = RawEventBusEnv {
            event_bus: std::env::var("EVENT_BUS").ok(),
            redpanda_brokers: std::env::var("REDPANDA_BROKERS").ok(),
            redpanda_topic: std::env::var("REDPANDA_TOPIC").ok(),
            pubsub_topic: std::env::var("PUBSUB_TOPIC").ok(),
            gcp_project_id: std::env::var("GCP_PROJECT_ID").ok(),
        };
        Self {
            database_url: std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            server_host: std::env::var("SERVER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            server_port: std::env::var("SERVER_PORT")
                .unwrap_or_else(|_| "8087".to_string())
                .parse()
                .expect("SERVER_PORT must be a valid u16"),
            log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
            event_bus: select_event_bus(EventBusEnv {
                event_bus: event.event_bus.as_deref(),
                redpanda_brokers: event.redpanda_brokers.as_deref(),
                redpanda_topic: event.redpanda_topic.as_deref(),
                pubsub_topic: event.pubsub_topic.as_deref(),
                gcp_project_id: event.gcp_project_id.as_deref(),
            })
            .unwrap_or_else(|e| panic!("{e}")),
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
        log_filter, parse_max_document_size_mb, select_event_bus, AppConfig, EventBus, EventBusEnv,
        DEFAULT_EVENT_TOPIC, DEFAULT_MAX_DOCUMENT_BYTES, DEFAULT_MAX_DOCUMENT_SIZE_MB,
    };

    fn config_with(mb: usize) -> AppConfig {
        AppConfig {
            database_url: String::new(),
            server_host: String::new(),
            server_port: 0,
            log_level: String::new(),
            event_bus: EventBus::Disabled,
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

    /// What the live Cloud Run revision actually sets, read back.
    ///
    /// Measured 2026-09-14 on `doceditor-00003-vkq`: `EVENT_BUS=pubsub`,
    /// `PUBSUB_TOPIC=editor-events`, `PUBSUB_TOPIC_DLQ=editor-events-dlq`,
    /// `GCP_PROJECT_ID=orbus-ods-staging`. Until this batch, not one of those
    /// four names appeared anywhere in `src/`.
    #[test]
    fn the_deployments_own_variables_select_the_pubsub_transport() {
        assert_eq!(
            select_event_bus(EventBusEnv {
                event_bus: Some("pubsub"),
                pubsub_topic: Some("editor-events"),
                gcp_project_id: Some("orbus-ods-staging"),
                ..EventBusEnv::default()
            }),
            Ok(EventBus::PubSub {
                project_id: "orbus-ods-staging".to_string(),
                topic: "editor-events".to_string(),
            })
        );
    }

    /// A deployment that names a transport but not what it needs **stops**.
    ///
    /// The alternative — falling back to the no-op producer — is the failure
    /// this service already lived through for four months, and it has no
    /// signature: the trait returns `Ok`, the callers check it, nothing is red.
    /// Refusing to boot is the only variant of "something is wrong" that a
    /// silent bus can be turned into.
    #[test]
    fn a_transport_that_cannot_be_honoured_refuses_the_boot() {
        let refusal = select_event_bus(EventBusEnv {
            event_bus: Some("pubsub"),
            pubsub_topic: Some("editor-events"),
            ..EventBusEnv::default()
        })
        .expect_err("pubsub without a project cannot publish anywhere");
        assert!(
            refusal.contains("GCP_PROJECT_ID"),
            "the refusal must name the variable to set; got {refusal:?}"
        );

        let refusal = select_event_bus(EventBusEnv {
            event_bus: Some("redpanda"),
            ..EventBusEnv::default()
        })
        .expect_err("redpanda without a broker cannot publish anywhere");
        assert!(refusal.contains("REDPANDA_BROKERS"), "got {refusal:?}");

        let refusal = select_event_bus(EventBusEnv {
            event_bus: Some("rabbitmq"),
            ..EventBusEnv::default()
        })
        .expect_err("an unknown transport is a typo, not a policy");
        assert!(
            refusal.contains("pubsub") && refusal.contains("rabbitmq"),
            "the refusal must name both the rejected value and the accepted ones; got {refusal:?}"
        );
    }

    /// The transport names people actually type.
    #[test]
    fn the_transport_is_named_case_insensitively_and_by_its_usual_aliases() {
        for spelling in ["pubsub", "PubSub", "PUBSUB", "pub/sub", "  pubsub  "] {
            assert!(
                matches!(
                    select_event_bus(EventBusEnv {
                        event_bus: Some(spelling),
                        gcp_project_id: Some("p"),
                        ..EventBusEnv::default()
                    }),
                    Ok(EventBus::PubSub { .. })
                ),
                "{spelling}"
            );
        }
        for spelling in ["kafka", "redpanda", "KAFKA"] {
            assert!(
                matches!(
                    select_event_bus(EventBusEnv {
                        event_bus: Some(spelling),
                        redpanda_brokers: Some("127.0.0.1:19092"),
                        ..EventBusEnv::default()
                    }),
                    Ok(EventBus::Redpanda { .. })
                ),
                "{spelling}"
            );
        }
    }

    /// The local-development path this repository has always had is unchanged:
    /// a broker address alone still selects Redpanda, and nothing at all still
    /// selects nothing.
    #[test]
    fn without_an_explicit_transport_a_broker_address_still_decides() {
        assert_eq!(
            select_event_bus(EventBusEnv {
                redpanda_brokers: Some("127.0.0.1:19092"),
                ..EventBusEnv::default()
            }),
            Ok(EventBus::Redpanda {
                brokers: "127.0.0.1:19092".to_string(),
                topic: DEFAULT_EVENT_TOPIC.to_string(),
            })
        );
        assert_eq!(
            select_event_bus(EventBusEnv::default()),
            Ok(EventBus::Disabled)
        );
        assert_eq!(
            select_event_bus(EventBusEnv {
                redpanda_brokers: Some("   "),
                ..EventBusEnv::default()
            }),
            Ok(EventBus::Disabled)
        );
    }

    /// Choosing no bus on purpose is allowed, and reads as such.
    #[test]
    fn a_deliberate_absence_of_bus_is_expressible() {
        for spelling in ["none", "noop", "off"] {
            assert_eq!(
                select_event_bus(EventBusEnv {
                    event_bus: Some(spelling),
                    redpanda_brokers: Some("127.0.0.1:19092"),
                    ..EventBusEnv::default()
                }),
                Ok(EventBus::Disabled),
                "{spelling}"
            );
        }
    }

    /// The canonical topic is the fallback on **both** transports.
    ///
    /// `editor-events` names the resource, not the carrier (spec.md §4.2), so a
    /// deployment that sets the transport and forgets the topic still publishes
    /// where consumers listen rather than somewhere plausible.
    #[test]
    fn the_canonical_topic_is_the_fallback_whatever_the_transport() {
        assert_eq!(
            select_event_bus(EventBusEnv {
                event_bus: Some("pubsub"),
                gcp_project_id: Some("orbus-ods-staging"),
                ..EventBusEnv::default()
            })
            .unwrap()
            .topic(),
            Some(DEFAULT_EVENT_TOPIC)
        );
        // And a deployment that only ever set REDPANDA_TOPIC keeps being obeyed
        // when it switches transport: the name is independent of the carrier.
        assert_eq!(
            select_event_bus(EventBusEnv {
                event_bus: Some("pubsub"),
                gcp_project_id: Some("orbus-ods-staging"),
                redpanda_topic: Some("editor-events"),
                ..EventBusEnv::default()
            })
            .unwrap()
            .topic(),
            Some("editor-events")
        );
        assert_eq!(EventBus::Disabled.topic(), None);
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
