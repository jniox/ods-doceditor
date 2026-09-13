//! The CloudEvents envelope, proven against a **real broker**.
//!
//! Why this file exists. `tests/../src/events/producer.rs` unit-tests
//! `kafka_record`, which builds the message. That proves the *envelope* is
//! right; it proves nothing about what leaves the process. Two claims sit
//! between the struct and a consumer, and only a round trip can settle them:
//!
//! - librdkafka actually puts the `ce_*` pairs on the wire as message headers
//!   (CloudEvents v1.0 **binary** content mode), rather than dropping them;
//! - `publish()` returning `Ok` means something. It does not, on its own:
//!   `send_result` enqueues and returns, so the caller's `Ok` is an *enqueue*
//!   receipt. Delivery is confirmed later, on a spawned task, and its failure
//!   is only logged. A test that stops at `assert!(result.is_ok())` is testing
//!   that a queue accepted a message.
//!
//! This is known limitation #2 of the GTM brief ("verify at least one
//! integration test publishes and reads a document lifecycle event from a
//! Redpanda instance — add if missing", owner: Code Agent) and the mitigation
//! of its risk row "CloudEvent emission silently broken (nil producer
//! pattern)". That risk was not hypothetical here: every event this service
//! produced between 2026-05-02 and 2026-09-13 went into a `NoopProducer` and
//! nothing failed. See ADR-002.
//!
//! Running it needs a broker. Locally:
//!
//! ```sh
//! docker run -d --name doceditor-redpanda-test -p 127.0.0.1:19092:19092 \
//!   docker.redpanda.com/redpandadata/redpanda:v24.2.7 \
//!   redpanda start --smp 1 --overprovisioned --node-id 0 --check=false \
//!     --mode dev-container --kafka-addr PLAINTEXT://0.0.0.0:19092 \
//!     --advertise-kafka-addr PLAINTEXT://127.0.0.1:19092
//! ```

mod common;

use ods_doceditor::events::producer::{kafka_record, CloudEvent, EventProducer, RedpandaProducer};
use rdkafka::admin::{AdminClient, AdminOptions, NewTopic, TopicReplication};
use rdkafka::client::DefaultClientContext;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{BaseConsumer, Consumer};
use rdkafka::message::{Headers, Message};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long to wait for a published message to come back. Generous because CI
/// starts the broker in the same job; a real failure still reports in seconds
/// because `message.timeout.ms` is 10s on the producer side.
const READ_DEADLINE: Duration = Duration::from_secs(30);

/// A topic per test run, so two runs (or two CI jobs) never read each other's
/// messages and a leftover message can never make an assertion pass.
fn unique_topic(label: &str) -> String {
    format!("doceditor-roundtrip-{label}-{}", Uuid::new_v4())
}

async fn create_topic(brokers: &str, topic: &str) {
    let admin: AdminClient<DefaultClientContext> = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .create()
        .expect("could not build the admin client");

    // Shorter than librdkafka's 60s default on purpose: the only way this call
    // times out is that there is no broker, and in that case the useful
    // behaviour is to say so quickly rather than to hold CI for a minute per
    // test while producing the same message.
    let results = admin
        .create_topics(
            &[NewTopic::new(topic, 1, TopicReplication::Fixed(1))],
            &AdminOptions::new().request_timeout(Some(Duration::from_secs(15))),
        )
        .await
        .unwrap_or_else(|e| {
            panic!(
                "could not reach the broker at {brokers} to create {topic}: {e}. \
                 This test does not skip without a broker — see tests/common/mod.rs."
            )
        });

    for result in results {
        result.unwrap_or_else(|(name, e)| panic!("broker refused to create {name}: {e}"));
    }
}

/// One message, flattened into the three things a consumer routes on.
struct Received {
    key: String,
    payload: String,
    headers: HashMap<String, String>,
}

/// Drain `topic` from its first offset until `expected` messages are read or
/// `deadline` passes. Returns whatever was read — the assertions, not this
/// helper, decide whether that is enough.
fn consume_for(brokers: &str, topic: &str, expected: usize, deadline: Duration) -> Vec<Received> {
    let consumer: BaseConsumer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set(
            "group.id",
            format!("doceditor-roundtrip-{}", Uuid::new_v4()),
        )
        .set("auto.offset.reset", "earliest")
        .set("enable.auto.commit", "false")
        .create()
        .expect("could not build the consumer");

    consumer
        .subscribe(&[topic])
        .expect("could not subscribe to the round-trip topic");

    let mut received = Vec::new();
    let started = Instant::now();
    while received.len() < expected && started.elapsed() < deadline {
        let Some(message) = consumer.poll(Duration::from_millis(250)) else {
            continue;
        };
        let message = message.expect("broker returned an error while consuming");

        let headers = message
            .headers()
            .map(|hs| {
                (0..hs.count())
                    .map(|i| {
                        let header = hs.get(i);
                        let value = header
                            .value
                            .map(|v| String::from_utf8_lossy(v).into_owned())
                            .unwrap_or_default();
                        (header.key.to_string(), value)
                    })
                    .collect()
            })
            .unwrap_or_default();

        received.push(Received {
            key: String::from_utf8_lossy(message.key().unwrap_or_default()).into_owned(),
            payload: String::from_utf8_lossy(message.payload().unwrap_or_default()).into_owned(),
            headers,
        });
    }
    received
}

/// The common case: wait up to `READ_DEADLINE` for the messages we published.
fn consume(brokers: &str, topic: &str, expected: usize) -> Vec<Received> {
    consume_for(brokers, topic, expected, READ_DEADLINE)
}

#[tokio::test]
async fn a_document_event_reaches_a_real_broker_with_its_cloudevents_headers_intact() {
    let brokers = common::broker_addr();
    let topic = unique_topic("created");
    create_topic(&brokers, &topic).await;

    let tenant_id = Uuid::new_v4();
    let document_id = Uuid::new_v4();
    let correlation_id = format!("corr-{}", Uuid::new_v4());
    let event = CloudEvent::document_created(
        tenant_id,
        document_id,
        "Contrat de prestation",
        Uuid::new_v4(),
    )
    .with_correlation_id(correlation_id.clone());
    let expected = kafka_record(&event);

    let producer = RedpandaProducer::new(&brokers, &topic).expect("could not build the producer");
    producer
        .publish(event.clone())
        .expect("the producer refused to enqueue the event");

    let received = consume(&brokers, &topic, 1);

    // Non-vacuity, first direction: the rest of this test asserts over
    // `received[0]`, and an empty vector would make every `for` below true by
    // vacuity. Nothing arriving is the exact failure this file exists to catch.
    assert_eq!(
        received.len(),
        1,
        "nothing came back from {topic} on {brokers} within {READ_DEADLINE:?}: \
         the event was enqueued but never delivered"
    );
    let message = &received[0];

    // Non-vacuity, second direction: a consumer that reads no headers at all
    // would satisfy any per-header comparison below.
    assert!(
        message.headers.len() >= expected.headers.len(),
        "the message came back with {} headers, fewer than the {} that were sent: {:?}",
        message.headers.len(),
        expected.headers.len(),
        message.headers
    );

    for (key, value) in &expected.headers {
        assert_eq!(
            message.headers.get(key),
            Some(value),
            "header {key} did not survive the round trip (got {:?})",
            message.headers.get(key)
        );
    }

    // The attributes a consumer routes on, named one by one rather than only
    // compared as a set, so a future change to `kafka_record` that drops one
    // cannot make this test agree with itself.
    assert_eq!(
        message.headers.get("ce_specversion").map(String::as_str),
        Some("1.0")
    );
    assert_eq!(
        message.headers.get("ce_type").map(String::as_str),
        Some("com.ods.editor.document.created")
    );
    assert_eq!(
        message.headers.get("ce_tenantid").map(String::as_str),
        Some(tenant_id.to_string().as_str()),
        "an event reached the bus without the tenant it belongs to"
    );
    assert_eq!(
        message.headers.get("ce_correlationid").map(String::as_str),
        Some(correlation_id.as_str())
    );
    assert_eq!(
        message.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );

    // Partition key: a document's events must stay ordered, which they only do
    // if they all land on the same partition (ADR-002, decision 2).
    assert_eq!(
        message.key,
        document_id.to_string(),
        "the message was keyed on something other than the document id"
    );

    // Binary content mode: the body is the event *data*, not the whole envelope.
    let payload: serde_json::Value =
        serde_json::from_str(&message.payload).expect("the payload is not JSON");
    assert_eq!(payload, event.data);
    assert!(
        payload.get("specversion").is_none(),
        "the envelope leaked into the body: this is structured mode, not binary mode"
    );
}

#[tokio::test]
async fn every_lifecycle_event_type_reaches_the_broker_under_its_own_ce_type() {
    let brokers = common::broker_addr();
    let topic = unique_topic("lifecycle");
    create_topic(&brokers, &topic).await;

    let tenant_id = Uuid::new_v4();
    let document_id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let events = vec![
        CloudEvent::document_created(tenant_id, document_id, "Titre", actor),
        CloudEvent::document_updated(tenant_id, document_id, actor, vec!["content"], 2),
        CloudEvent::document_published(tenant_id, document_id, actor, 2),
        CloudEvent::version_created(tenant_id, document_id, 3, actor, false),
        CloudEvent::document_deleted(tenant_id, document_id, actor),
    ];
    let expected_types: Vec<String> = events.iter().map(|e| e.event_type.clone()).collect();

    let producer = RedpandaProducer::new(&brokers, &topic).expect("could not build the producer");
    for event in events {
        producer
            .publish(event)
            .expect("the producer refused to enqueue");
    }

    let received = consume(&brokers, &topic, expected_types.len());

    assert_eq!(
        received.len(),
        expected_types.len(),
        "{} of {} lifecycle events came back",
        received.len(),
        expected_types.len()
    );

    let seen: Vec<String> = received
        .iter()
        .map(|m| {
            m.headers
                .get("ce_type")
                .cloned()
                .unwrap_or_else(|| "<no ce_type header>".to_string())
        })
        .collect();
    assert_eq!(
        seen, expected_types,
        "the lifecycle events did not arrive, in order, under their own types"
    );

    // One document, one partition: the order asserted above is only meaningful
    // because every message carries the same key (ADR-002, decision 2).
    for message in &received {
        assert_eq!(message.key, document_id.to_string());
    }
}

/// The companion that makes the two tests above worth their runtime.
///
/// If `consume` returned messages regardless of what was published — a stale
/// subscription, a misread topic name, a helper that fabricates on timeout —
/// both tests above would pass without anything having been delivered. This
/// one publishes nothing and requires the consumer to come back empty.
#[tokio::test]
async fn the_consumer_reads_nothing_from_a_topic_nobody_published_to() {
    let brokers = common::broker_addr();
    let topic = unique_topic("silent");
    create_topic(&brokers, &topic).await;

    // A short window on purpose: this test asserts an *absence*, so its cost is
    // pure waiting. Long enough that a delivery in flight would be seen (the
    // two tests above routinely come back in well under a second), short enough
    // that it does not dominate the suite.
    let received = consume_for(&brokers, &topic, 1, Duration::from_secs(5));

    assert!(
        received.is_empty(),
        "{} message(s) came back from a topic nothing was published to — \
         the round-trip assertions in this file prove nothing",
        received.len()
    );
}
