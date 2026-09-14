use chrono::{DateTime, Utc};
use rdkafka::config::ClientConfig;
use rdkafka::message::{Header, OwnedHeaders};
use rdkafka::producer::{FutureProducer, FutureRecord};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// CloudEvents v1.0 envelope for editor domain events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudEvent {
    pub specversion: String,
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub time: DateTime<Utc>,
    pub datacontenttype: String,
    pub tenantid: String,
    /// CloudEvents extension attribute: the `X-Correlation-Id` of the request
    /// that caused this event, so a trace can be followed from an HTTP call
    /// into the analytics pipeline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlationid: Option<String>,
    pub data: serde_json::Value,
}

impl CloudEvent {
    pub fn new(event_type: &str, tenant_id: Uuid, data: serde_json::Value) -> Self {
        Self {
            specversion: "1.0".to_string(),
            id: format!("evt-{}", Uuid::new_v4()),
            source: "/editor".to_string(),
            event_type: event_type.to_string(),
            time: Utc::now(),
            datacontenttype: "application/json".to_string(),
            tenantid: tenant_id.to_string(),
            correlationid: None,
            data,
        }
    }

    /// Attach the correlation id of the request that caused this event.
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlationid = Some(correlation_id.into());
        self
    }

    pub fn document_created(
        tenant_id: Uuid,
        document_id: Uuid,
        title: &str,
        created_by: Uuid,
    ) -> Self {
        Self::new(
            "com.ods.editor.document.created",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "title": title,
                "created_by": created_by,
            }),
        )
    }

    pub fn document_updated(
        tenant_id: Uuid,
        document_id: Uuid,
        updated_by: Uuid,
        changes: Vec<&str>,
        version: i32,
    ) -> Self {
        Self::new(
            "com.ods.editor.document.updated",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "updated_by": updated_by,
                "changes": changes,
                "version": version,
            }),
        )
    }

    pub fn document_deleted(tenant_id: Uuid, document_id: Uuid, deleted_by: Uuid) -> Self {
        Self::new(
            "com.ods.editor.document.deleted",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "deleted_by": deleted_by,
            }),
        )
    }

    pub fn document_published(
        tenant_id: Uuid,
        document_id: Uuid,
        published_by: Uuid,
        version: i32,
    ) -> Self {
        Self::new(
            "com.ods.editor.document.published",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "published_by": published_by,
                "version": version,
            }),
        )
    }

    pub fn version_created(
        tenant_id: Uuid,
        document_id: Uuid,
        version: i32,
        created_by: Uuid,
        is_auto: bool,
    ) -> Self {
        Self::new(
            "com.ods.editor.version.created",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "version": version,
                "created_by": created_by,
                "is_auto": is_auto,
            }),
        )
    }
}

/// One Kafka message, built from a CloudEvent, ready to hand to librdkafka.
///
/// Split out from the producer itself so the binding can be tested without a
/// broker: "the envelope is correct" and "the broker accepted it" are two
/// different claims and only the first is worth a unit test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KafkaRecord {
    /// Partition key. The document id, so a document's events keep their order.
    pub key: String,
    pub payload: String,
    pub headers: Vec<(String, String)>,
}

/// The CloudEvents attributes of an event, under their **unprefixed** spec
/// names, in the order a binding writes them.
///
/// One source, because there are now two bindings of the same envelope: Kafka
/// headers prefix these with `ce_`, Pub/Sub message attributes with `ce-`. Two
/// hand-written lists drift the first time an attribute is added to one of
/// them, and an envelope missing an attribute fails nothing — the bus accepts
/// it and a consumer quietly reads `None`. Same shape as `version_repo`'s two
/// column lists and `document_repo::search_predicate`: one definition, several
/// renderings. `tests/pubsub_transport_test.rs` holds the two against each
/// other.
///
/// `datacontenttype` is deliberately not here: both bindings carry it under the
/// transport's own name (`content-type`) rather than as a `ce_`/`ce-`
/// attribute, which is what the CloudEvents binary content mode prescribes.
pub fn cloudevent_attributes(event: &CloudEvent) -> Vec<(&'static str, String)> {
    let mut attributes = vec![
        ("specversion", event.specversion.clone()),
        ("id", event.id.clone()),
        ("source", event.source.clone()),
        ("type", event.event_type.clone()),
        ("time", event.time.to_rfc3339()),
        ("tenantid", event.tenantid.clone()),
    ];
    if let Some(correlation_id) = &event.correlationid {
        attributes.push(("correlationid", correlation_id.clone()));
    }
    attributes
}

/// The value that keeps one document's events in order, whatever the transport
/// calls it: a Kafka partition key, a Pub/Sub ordering key.
pub fn partition_key(event: &CloudEvent) -> String {
    event
        .data
        .get("document_id")
        .and_then(|v| v.as_str())
        .unwrap_or(&event.id)
        .to_string()
}

/// CloudEvents v1.0 *binary* content mode for Kafka: the attributes travel as
/// `ce_*` message headers and the body carries the event data. This is what
/// the platform rule "CloudEvents v1.0 in message attributes" means on a
/// broker that has headers rather than attributes.
pub fn kafka_record(event: &CloudEvent) -> KafkaRecord {
    let mut headers: Vec<(String, String)> = cloudevent_attributes(event)
        .into_iter()
        .map(|(name, value)| (format!("ce_{name}"), value))
        .collect();
    headers.push(("content-type".to_string(), event.datacontenttype.clone()));

    KafkaRecord {
        key: partition_key(event),
        payload: event.data.to_string(),
        headers,
    }
}

/// Trait for event publishing (allows mocking in tests).
pub trait EventProducer: Send + Sync {
    fn publish(&self, event: CloudEvent) -> Result<(), String>;

    /// Which implementation this is, so startup logs and tests can tell a real
    /// producer from the one that throws events away.
    fn name(&self) -> &'static str;
}

/// In-memory producer for testing.
#[derive(Debug, Default, Clone)]
pub struct InMemoryProducer {
    pub events: Arc<Mutex<Vec<CloudEvent>>>,
}

impl InMemoryProducer {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn get_events(&self) -> Vec<CloudEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl EventProducer for InMemoryProducer {
    fn publish(&self, event: CloudEvent) -> Result<(), String> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "in-memory"
    }
}

/// No-op producer when no broker is configured.
///
/// It must stay reachable only through an explicit absence of any bus
/// configuration: wiring it unconditionally is how this service spent four
/// months building correct CloudEvents and delivering none of them.
pub struct NoopProducer;

impl EventProducer for NoopProducer {
    fn publish(&self, event: CloudEvent) -> Result<(), String> {
        tracing::debug!(
            event_type = %event.event_type,
            "Event dropped: no event bus configured (EVENT_BUS/REDPANDA_BROKERS unset)"
        );
        Ok(())
    }

    fn name(&self) -> &'static str {
        "noop"
    }
}

/// Publishes to Redpanda/Kafka.
pub struct RedpandaProducer {
    producer: FutureProducer,
    topic: String,
}

impl RedpandaProducer {
    pub fn new(brokers: &str, topic: &str) -> Result<Self, String> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            // Bounded, so a broker outage degrades into logged drops rather
            // than unbounded memory growth in the API process.
            .set("queue.buffering.max.messages", "100000")
            .set("message.timeout.ms", "10000")
            .set("compression.type", "lz4")
            .create()
            .map_err(|e| format!("Failed to create the Redpanda producer: {e}"))?;

        Ok(Self {
            producer,
            topic: topic.to_string(),
        })
    }
}

impl EventProducer for RedpandaProducer {
    fn publish(&self, event: CloudEvent) -> Result<(), String> {
        let record = kafka_record(&event);

        let mut headers = OwnedHeaders::new();
        for (key, value) in &record.headers {
            headers = headers.insert(Header {
                key,
                value: Some(value),
            });
        }

        let future_record = FutureRecord::to(&self.topic)
            .key(&record.key)
            .payload(&record.payload)
            .headers(headers);

        // `send_result` enqueues and returns immediately: publishing must never
        // hold an HTTP request hostage to broker latency. Delivery is confirmed
        // asynchronously, and a failure is logged rather than swallowed.
        match self.producer.send_result(future_record) {
            Ok(delivery) => {
                let event_type = event.event_type.clone();
                let event_id = event.id.clone();
                tokio::spawn(async move {
                    match delivery.await {
                        Ok(Ok(_)) => {}
                        Ok(Err((e, _))) => tracing::error!(
                            event_id = %event_id,
                            event_type = %event_type,
                            "Event rejected by the broker: {e}"
                        ),
                        Err(e) => tracing::error!(
                            event_id = %event_id,
                            event_type = %event_type,
                            "Event delivery was cancelled: {e}"
                        ),
                    }
                });
                Ok(())
            }
            Err((e, _)) => Err(format!("Failed to enqueue the event: {e}")),
        }
    }

    fn name(&self) -> &'static str {
        "redpanda"
    }
}

/// Build the producer the configuration selected.
///
/// The selection itself lives in `config::select_event_bus`, which is a pure
/// function over the environment and refuses a choice it cannot honour: an
/// operator who writes `EVENT_BUS=pubsub` without `GCP_PROJECT_ID` stops the
/// boot rather than being served a producer that drops everything. That matters
/// more here than anywhere else in this service, because a dropped event has
/// **no failure signature at all** — the trait returns `Ok`, the callers check
/// it, and four months pass (ADR-002).
///
/// The no-op producer is therefore reachable only through a deliberate absence
/// of any bus configuration, and it says so at WARN.
pub fn producer_from_config(bus: &crate::config::EventBus) -> Arc<dyn EventProducer> {
    use crate::config::EventBus;

    match bus {
        EventBus::Redpanda { brokers, topic } => match RedpandaProducer::new(brokers, topic) {
            Ok(producer) => {
                tracing::info!(brokers, topic, "Publishing events to Redpanda");
                Arc::new(producer)
            }
            Err(e) => {
                tracing::error!(
                    "Could not build the Redpanda producer ({e}); events will be dropped"
                );
                Arc::new(NoopProducer)
            }
        },
        EventBus::PubSub { project_id, topic } => {
            let endpoints = crate::events::pubsub::endpoints_from_env(
                std::env::var(crate::events::pubsub::EMULATOR_HOST_VAR)
                    .ok()
                    .as_deref(),
                std::env::var(crate::events::pubsub::METADATA_HOST_VAR)
                    .ok()
                    .as_deref(),
            );
            match crate::events::pubsub::PubSubProducer::new(project_id, topic, endpoints) {
                Ok(producer) => {
                    tracing::info!(
                        project_id,
                        topic,
                        publish_url = producer.publish_url(),
                        "Publishing events to Cloud Pub/Sub"
                    );
                    Arc::new(producer)
                }
                Err(e) => {
                    tracing::error!(
                        "Could not build the Pub/Sub producer ({e}); events will be dropped"
                    );
                    Arc::new(NoopProducer)
                }
            }
        }
        EventBus::Disabled => {
            // Deliberately does not name *which* variable is missing: this
            // arm is reached both by an empty environment and by an explicit
            // `EVENT_BUS=none`, and a startup line that states a false cause is
            // the thing this repository keeps having to unpick. What is
            // asserted is the consequence, and the remedy.
            tracing::warn!(
                "No event bus is selected: document events will be DROPPED, not published. \
                 Set EVENT_BUS=pubsub with GCP_PROJECT_ID (staging), or REDPANDA_BROKERS \
                 (local broker), to publish."
            );
            Arc::new(NoopProducer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> CloudEvent {
        CloudEvent::document_created(
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            "Contrat",
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
        )
    }

    #[test]
    fn kafka_record_carries_every_mandatory_cloudevents_attribute() {
        let record = kafka_record(&sample_event());
        let keys: Vec<&str> = record.headers.iter().map(|(k, _)| k.as_str()).collect();

        for required in [
            "ce_specversion",
            "ce_id",
            "ce_source",
            "ce_type",
            "ce_time",
            "ce_tenantid",
        ] {
            assert!(keys.contains(&required), "missing header {required}");
        }

        let header = |name: &str| {
            record
                .headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(header("ce_specversion"), "1.0");
        assert_eq!(header("ce_type"), "com.ods.editor.document.created");
        assert_eq!(
            header("ce_tenantid"),
            "11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn kafka_record_partitions_by_document_so_a_history_stays_ordered() {
        let record = kafka_record(&sample_event());
        assert_eq!(record.key, "22222222-2222-2222-2222-222222222222");
    }

    #[test]
    fn kafka_record_payload_is_the_event_data() {
        let event = sample_event();
        let record = kafka_record(&event);
        let payload: serde_json::Value = serde_json::from_str(&record.payload).unwrap();
        assert_eq!(payload["title"], "Contrat");
        assert_eq!(payload, event.data);
    }

    #[test]
    fn a_correlated_event_carries_its_correlation_id_on_the_wire() {
        let event = sample_event().with_correlation_id("corr-42");
        let record = kafka_record(&event);
        assert!(record
            .headers
            .contains(&("ce_correlationid".to_string(), "corr-42".to_string())));
    }

    #[actix_web::test]
    async fn configured_brokers_select_the_real_producer() {
        // librdkafka connects lazily, so this builds without a broker running.
        let producer = producer_from_config(&crate::config::EventBus::Redpanda {
            brokers: "127.0.0.1:9092".to_string(),
            topic: crate::config::DEFAULT_EVENT_TOPIC.to_string(),
        });
        assert_eq!(producer.name(), "redpanda");
    }

    /// The transport the deployment chose (HR-20260914-007, option A).
    #[actix_web::test]
    async fn a_pubsub_bus_selects_the_pubsub_producer() {
        let producer = producer_from_config(&crate::config::EventBus::PubSub {
            project_id: "orbus-ods-staging".to_string(),
            topic: crate::config::DEFAULT_EVENT_TOPIC.to_string(),
        });
        assert_eq!(producer.name(), "pubsub");
    }

    #[actix_web::test]
    async fn no_configured_bus_falls_back_to_the_noop_producer() {
        assert_eq!(
            producer_from_config(&crate::config::EventBus::Disabled).name(),
            "noop"
        );
    }
}
