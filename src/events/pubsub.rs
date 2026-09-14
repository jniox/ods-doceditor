//! Publishing to Google Cloud Pub/Sub — the transport the deployment chose.
//!
//! **Why this module exists.** `doceditor` builds correct CloudEvents and has
//! since May 2026 delivered none of them in staging: the code published with
//! `rdkafka`, and no Redpanda broker exists in `orbus-ods-staging`, so the
//! `NoopProducer` was selected and every event was dropped — silently, which is
//! this service's documented failure mode (ADR-002). Meanwhile the live Cloud
//! Run revision already carried `EVENT_BUS=pubsub`, `PUBSUB_TOPIC=editor-events`
//! and `GCP_PROJECT_ID`: **the deployment had chosen a transport and the code
//! had never learned it.** Settled by HR-20260914-007 (option A, 2026-09-14,
//! it@orbusdigital.com, *"Exécutant : dev, dans le dépôt doceditor"*). See
//! ADR-011.
//!
//! **REST and not gRPC, on purpose.** Every gRPC client for Pub/Sub pulls
//! `tonic` and therefore `h2` — the crate this estate is under a platform-wide
//! decision to keep out of its shipped graph (HR-20260909-001,
//! RUSTSEC-2026-0258). The publish call is one HTTPS POST; paying for a
//! transitive HTTP/2 stack to make it would undo a decision taken across ten
//! repositories. `tests/framework.rs` measures the result rather than trusting
//! this paragraph.
//!
//! **Credentials.** On Cloud Run the runtime service account's access token is
//! served by the instance metadata server; nothing is stored in this repository
//! and no key file is read. Measured 2026-09-14:
//! `runtime-cloud-run@orbus-ods-staging.iam.gserviceaccount.com` already holds
//! `roles/pubsub.publisher` at project level, so option A needed no
//! infrastructure change — which is what made it the recommended option.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;

use super::producer::{cloudevent_attributes, CloudEvent, EventProducer};

/// Where a Pub/Sub access token comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PubSubAuth {
    /// The instance metadata server, which is what Cloud Run provides. `base`
    /// is a full origin (`http://metadata.google.internal`).
    MetadataServer { base: String },
    /// No credentials at all — the Pub/Sub emulator, and only it.
    Emulator,
}

/// The two origins a Pub/Sub publisher talks to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubSubEndpoints {
    /// Origin of the Pub/Sub API, without a trailing slash.
    pub publish_base: String,
    pub auth: PubSubAuth,
}

/// Google's own environment knob for an unauthenticated local emulator.
pub const EMULATOR_HOST_VAR: &str = "PUBSUB_EMULATOR_HOST";
/// Google's own environment knob for redirecting the metadata server.
pub const METADATA_HOST_VAR: &str = "GCE_METADATA_HOST";

const DEFAULT_PUBSUB_BASE: &str = "https://pubsub.googleapis.com";
const DEFAULT_METADATA_HOST: &str = "metadata.google.internal";

/// Resolve the endpoints from the two standard Google variables.
///
/// Neither is invented here: `PUBSUB_EMULATOR_HOST` and `GCE_METADATA_HOST` are
/// the names every Google client library reads, so a developer who already has
/// an emulator running needs to learn nothing from this service.
pub fn endpoints_from_env(
    emulator_host: Option<&str>,
    metadata_host: Option<&str>,
) -> PubSubEndpoints {
    fn non_blank(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }

    if let Some(host) = non_blank(emulator_host) {
        return PubSubEndpoints {
            publish_base: with_scheme(host, "http"),
            auth: PubSubAuth::Emulator,
        };
    }
    PubSubEndpoints {
        publish_base: DEFAULT_PUBSUB_BASE.to_string(),
        auth: PubSubAuth::MetadataServer {
            base: with_scheme(
                non_blank(metadata_host).unwrap_or(DEFAULT_METADATA_HOST),
                "http",
            ),
        },
    }
}

/// Both Google variables are documented as `host[:port]`, but people paste
/// URLs. Accept either rather than producing `http://http://…`.
fn with_scheme(host: &str, default_scheme: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("{default_scheme}://{host}")
    }
}

/// One Pub/Sub message, built from a CloudEvent, ready to be serialised.
///
/// Split out from the producer for the same reason `KafkaRecord` is: "the
/// envelope is correct" and "the bus accepted it" are two claims, and only the
/// first is a unit test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubSubMessage {
    /// The partitioning value — the document id, so a document's events keep
    /// their order. Pub/Sub calls it an ordering key; Kafka calls it the
    /// partition key; it is the same value.
    pub ordering_key: String,
    /// The event data, base64-encoded as the Pub/Sub REST API requires.
    pub data: String,
    pub attributes: Vec<(String, String)>,
}

/// CloudEvents v1.0 **binary content mode** on Pub/Sub: the envelope travels as
/// message attributes and the body carries the data alone.
///
/// The attribute names are the HTTP binding's — `ce-` and not Kafka's `ce_` —
/// and that is not cosmetic. The platform delivers by **push subscription to
/// Cloud Run** (ADR-003); a push subscription with payload unwrapping and
/// metadata writing turns each attribute into an HTTP header verbatim, so
/// `ce-type` arrives as the header a standard CloudEvents HTTP consumer reads,
/// while `ce_type` would arrive as a header no SDK looks for.
pub fn pubsub_message(event: &CloudEvent) -> PubSubMessage {
    let mut attributes: Vec<(String, String)> = cloudevent_attributes(event)
        .into_iter()
        .map(|(name, value)| (format!("ce-{name}"), value))
        .collect();
    attributes.push(("content-type".to_string(), event.datacontenttype.clone()));

    PubSubMessage {
        ordering_key: super::producer::partition_key(event),
        data: base64::engine::general_purpose::STANDARD.encode(event.data.to_string()),
        attributes,
    }
}

/// An access token and the moment it stops being usable.
#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    good_until: Instant,
}

/// Everything a publish needs, shared by the request path and the spawned
/// tasks — including the token cache, which is the point: a cache that each
/// task copies is not a cache, it is one metadata-server call per event.
struct PubSubClient {
    client: reqwest::Client,
    /// The full publish URL, built once: a topic name that is wrong is silent,
    /// so it is resolved at startup and logged there rather than per event.
    publish_url: String,
    auth: PubSubAuth,
    token: Mutex<Option<CachedToken>>,
}

/// Publishes CloudEvents to a Pub/Sub topic over the REST API.
pub struct PubSubProducer {
    inner: Arc<PubSubClient>,
    topic: String,
}

impl PubSubProducer {
    /// The same ceiling `RedpandaProducer` gives librdkafka
    /// (`message.timeout.ms`): a bus that stops answering must degrade into
    /// logged failures, never into tasks that accumulate.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    /// Renew a little before expiry, so a token never expires mid-flight.
    const RENEW_MARGIN: Duration = Duration::from_secs(60);

    pub fn new(project_id: &str, topic: &str, endpoints: PubSubEndpoints) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Self::REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("Failed to build the Pub/Sub HTTP client: {e}"))?;

        Ok(Self {
            inner: Arc::new(PubSubClient {
                client,
                publish_url: format!(
                    "{}/v1/projects/{project_id}/topics/{topic}:publish",
                    endpoints.publish_base.trim_end_matches('/')
                ),
                auth: endpoints.auth,
                token: Mutex::new(None),
            }),
            topic: topic.to_string(),
        })
    }

    /// The URL events are actually sent to — logged at startup, because a name
    /// nobody consumes fails nothing.
    pub fn publish_url(&self) -> &str {
        &self.inner.publish_url
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Publish one event and wait for the bus to answer.
    ///
    /// `publish` (the trait method) spawns this; tests await it, which is the
    /// only way to assert on the answer.
    pub async fn publish_once(&self, event: &CloudEvent) -> Result<(), String> {
        self.inner.publish_once(event).await
    }
}

impl PubSubClient {
    async fn publish_once(&self, event: &CloudEvent) -> Result<(), String> {
        let message = pubsub_message(event);
        let attributes: serde_json::Map<String, serde_json::Value> = message
            .attributes
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let body = serde_json::json!({
            "messages": [{
                "data": message.data,
                "attributes": attributes,
                "orderingKey": message.ordering_key,
            }]
        });

        let mut request = self.client.post(&self.publish_url).json(&body);
        if let Some(token) = self.access_token().await? {
            request = request.bearer_auth(token);
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("Could not reach Pub/Sub at {}: {e}", self.publish_url))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // The body carries Google's reason (a missing `roles/pubsub.publisher`
        // reads as PERMISSION_DENIED); truncated because it is going into a log
        // line, not into a response.
        let detail: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect();
        Err(format!("Pub/Sub refused the event: {status} — {detail}"))
    }

    /// The bearer token to present, or `None` when talking to an emulator.
    async fn access_token(&self) -> Result<Option<String>, String> {
        let PubSubAuth::MetadataServer { base } = &self.auth else {
            return Ok(None);
        };

        // Read the cache without holding the lock across an await: the guard is
        // taken and dropped inside this block, before the request below.
        {
            let cached = self.token.lock().unwrap();
            if let Some(cached) = cached.as_ref() {
                if Instant::now() < cached.good_until {
                    return Ok(Some(cached.value.clone()));
                }
            }
        }

        let url = format!("{base}/computeMetadata/v1/instance/service-accounts/default/token");
        let response = self
            .client
            .get(&url)
            // Not optional: the metadata server refuses any request that does
            // not carry it, which is how it knows the caller is not a browser
            // that was tricked into asking.
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|e| format!("Could not reach the instance metadata server at {url}: {e}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "The instance metadata server refused to issue an access token: {}",
                response.status()
            ));
        }

        #[derive(serde::Deserialize)]
        struct TokenResponse {
            access_token: String,
            #[serde(default)]
            expires_in: u64,
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| format!("The instance metadata server returned no usable token: {e}"))?;

        let lifetime =
            Duration::from_secs(token.expires_in).saturating_sub(PubSubProducer::RENEW_MARGIN);
        *self.token.lock().unwrap() = Some(CachedToken {
            value: token.access_token.clone(),
            good_until: Instant::now() + lifetime,
        });
        Ok(Some(token.access_token))
    }
}

impl EventProducer for PubSubProducer {
    fn publish(&self, event: CloudEvent) -> Result<(), String> {
        // Enqueue and return, exactly as the Kafka producer does: publishing
        // must never hold an HTTP request hostage to bus latency. The failure
        // is logged rather than swallowed — that is the whole difference
        // between this and the no-op producer.
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let event_id = event.id.clone();
            let event_type = event.event_type.clone();
            if let Err(e) = inner.publish_once(&event).await {
                tracing::error!(
                    event_id = %event_id,
                    event_type = %event_type,
                    "Event not published: {e}"
                );
            }
        });
        Ok(())
    }

    fn name(&self) -> &'static str {
        "pubsub"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sample_event() -> CloudEvent {
        CloudEvent::document_created(
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            "Contrat",
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        )
    }

    fn attribute(message: &PubSubMessage, name: &str) -> Option<String> {
        message
            .attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn the_envelope_travels_as_attributes_prefixed_the_http_bindings_way() {
        let message = pubsub_message(&sample_event());
        for required in [
            "ce-specversion",
            "ce-id",
            "ce-source",
            "ce-type",
            "ce-time",
            "ce-tenantid",
        ] {
            assert!(
                attribute(&message, required).is_some(),
                "missing attribute {required}"
            );
        }
        assert_eq!(attribute(&message, "ce-specversion").unwrap(), "1.0");
        assert_eq!(
            attribute(&message, "ce-type").unwrap(),
            "com.ods.editor.document.created"
        );
        assert_eq!(
            attribute(&message, "content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn the_data_is_the_event_data_alone_base64_encoded() {
        let event = sample_event();
        let message = pubsub_message(&event);
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&message.data)
            .unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(decoded, event.data);
    }

    #[test]
    fn the_ordering_key_is_the_document_so_a_history_stays_ordered() {
        assert_eq!(
            pubsub_message(&sample_event()).ordering_key,
            "22222222-2222-2222-2222-222222222222"
        );
    }

    #[test]
    fn a_correlated_event_carries_its_correlation_id() {
        let message = pubsub_message(&sample_event().with_correlation_id("corr-7"));
        assert_eq!(attribute(&message, "ce-correlationid").unwrap(), "corr-7");
    }

    #[test]
    fn the_publish_url_names_the_project_and_the_topic() {
        let producer = PubSubProducer::new(
            "orbus-ods-staging",
            "editor-events",
            endpoints_from_env(None, None),
        )
        .unwrap();
        assert_eq!(
            producer.publish_url(),
            "https://pubsub.googleapis.com/v1/projects/orbus-ods-staging/topics/editor-events:publish"
        );
    }

    /// The emulator variable is Google's, and people paste both shapes into it.
    #[test]
    fn the_emulator_variable_is_accepted_as_a_host_or_as_a_url() {
        for raw in ["127.0.0.1:8085", "http://127.0.0.1:8085", "127.0.0.1:8085/"] {
            let endpoints = endpoints_from_env(Some(raw), None);
            assert_eq!(endpoints.publish_base, "http://127.0.0.1:8085", "{raw}");
            assert_eq!(endpoints.auth, PubSubAuth::Emulator);
        }
    }

    #[test]
    fn without_an_emulator_the_token_comes_from_the_instance_metadata_server() {
        let endpoints = endpoints_from_env(Some("  "), None);
        assert_eq!(endpoints.publish_base, DEFAULT_PUBSUB_BASE);
        assert_eq!(
            endpoints.auth,
            PubSubAuth::MetadataServer {
                base: "http://metadata.google.internal".to_string()
            }
        );
    }
}
