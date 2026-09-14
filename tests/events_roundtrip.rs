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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// How long to wait for a published message to come back. Generous because CI
/// starts the broker in the same job; a real failure still reports in seconds
/// because `message.timeout.ms` is 10s on the producer side.
const READ_DEADLINE: Duration = Duration::from_secs(30);

/// The single command that makes this file runnable on a machine that has no
/// broker — and the one the ADLC pipeline's host was missing on 2026-09-13.
///
/// It is `--restart unless-stopped` and not a throwaway container on purpose:
/// unlike CI, which starts its own broker per run (`.github/workflows/ci.yml`),
/// the pipeline runs `cargo test --all` on a long-lived host with no
/// environment of its own. A broker removed after a local run leaves the suite
/// red there, for an infrastructure reason that reads like a code regression.
const START_A_BROKER: &str = concat!(
    "docker run -d --name doceditor-redpanda-dev --restart unless-stopped ",
    "-p 127.0.0.1:19092:19092 docker.redpanda.com/redpandadata/redpanda:v24.2.7 ",
    "redpanda start --smp 1 --overprovisioned --node-id 0 --check=false --mode dev-container ",
    "--kafka-addr PLAINTEXT://0.0.0.0:19092 --advertise-kafka-addr PLAINTEXT://127.0.0.1:19092",
);

/// The whole diagnosis in one line: what was unreachable, what it refused, that
/// this is not a test that skips, and how to make it runnable.
fn unreachable_broker(brokers: &str, topic: &str, error: &str) -> String {
    format!(
        "could not reach the broker at {brokers} to create {topic}: {error}. \
         This test does not skip without a broker — see tests/common/mod.rs and ADR-002. \
         Start one and re-run: {START_A_BROKER}"
    )
}

/// The prefix every topic this file creates carries — and the **only** thing
/// the sweep below is ever allowed to match. The standing broker is shared with
/// the service's own topic (`editor.events`) and with the cluster's internals;
/// a sweep with a looser rule would be far worse than the leak it repairs.
const TOPIC_PREFIX: &str = "doceditor-roundtrip-";

/// How old a leftover topic must be before a *later* run deletes it.
///
/// Three orders of magnitude above this file's own runtime (~6s), so a sweep
/// can never take a topic out from under a run still in progress — including a
/// second, concurrent run against the same standing broker.
const STALE_AFTER_SECS: u64 = 3600;

/// Bound on every admin round trip. Same reasoning as the 15s on create: the
/// only way these time out is that there is no broker.
const TOPIC_OP_TIMEOUT: Duration = Duration::from_secs(15);

/// The remedy for a broker that is already saturated, for the reader of a
/// failure rather than for the code: the sweep below handles this by itself
/// from now on, but a broker filled by *older* runs is still there today.
const PRUNE_TOPICS: &str = concat!(
    "docker exec doceditor-redpanda-dev rpk topic list --brokers 127.0.0.1:19092 ",
    "| awk '/^doceditor-roundtrip-/ {print $1}' ",
    "| xargs -r docker exec doceditor-redpanda-dev rpk topic delete --brokers 127.0.0.1:19092",
);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A topic per test run, so two runs (or two CI jobs) never read each other's
/// messages and a leftover message can never make an assertion pass.
///
/// The creation time leads the name because it is what a *later* run needs:
/// a guard deletes its own topic, but a process that is killed — or a test that
/// panics before its guard exists — cannot. Parsing one leading integer is
/// unambiguous; a timestamp buried between a free-form label and a UUID is not.
fn topic_name(label: &str, created_at_secs: u64) -> String {
    format!("{TOPIC_PREFIX}{created_at_secs}-{label}-{}", Uuid::new_v4())
}

/// Whether `topic` is a leftover of an *earlier* run of this file.
///
/// Two refusals matter more than the arithmetic: a topic that does not carry
/// this file's prefix is never stale (it belongs to the service, to another
/// service, or to the cluster), and a clock that moved backwards cannot make a
/// live topic look ancient — hence the saturating subtraction.
fn topic_is_stale(topic: &str, now_secs: u64) -> bool {
    let Some(rest) = topic.strip_prefix(TOPIC_PREFIX) else {
        return false;
    };
    match rest
        .split('-')
        .next()
        .and_then(|head| head.parse::<u64>().ok())
    {
        Some(created_at) => now_secs.saturating_sub(created_at) >= STALE_AFTER_SECS,
        // A name from before this file stamped its topics: an older run's, by
        // construction, since every run since carries a timestamp.
        None => true,
    }
}

fn admin_client(brokers: &str) -> AdminClient<DefaultClientContext> {
    ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .create()
        .expect("could not build the admin client")
}

fn admin_options() -> AdminOptions {
    AdminOptions::new().request_timeout(Some(TOPIC_OP_TIMEOUT))
}

fn topic_names(admin: &AdminClient<DefaultClientContext>) -> Vec<String> {
    admin
        .inner()
        .fetch_metadata(None, TOPIC_OP_TIMEOUT)
        .map(|m| m.topics().iter().map(|t| t.name().to_string()).collect())
        .unwrap_or_default()
}

/// Delete what earlier runs of this file left behind, and say how many.
///
/// This is the half of the teardown that survives a `kill -9`: the guard below
/// covers the normal and the panicking path, this one covers everything else,
/// so a broker that has been saturated by past runs *heals on the next run*
/// instead of refusing every create until a human re-derives why.
async fn prune_stale_topics(admin: &AdminClient<DefaultClientContext>, now_secs: u64) -> usize {
    let stale: Vec<String> = topic_names(admin)
        .into_iter()
        .filter(|name| topic_is_stale(name, now_secs))
        .collect();
    if stale.is_empty() {
        return 0;
    }
    let names: Vec<&str> = stale.iter().map(String::as_str).collect();
    match admin.delete_topics(&names, &admin_options()).await {
        Ok(results) => results.into_iter().filter(|r| r.is_ok()).count(),
        Err(_) => 0,
    }
}

/// What a reader is told when the broker refuses to create a topic.
///
/// The refusal that actually happens here is `InvalidPartitions`, and read
/// alone it is indistinguishable from a code regression — it is what the BA's
/// 2026-09-14 cycle chased for a full turn. This broker allocates one file
/// descriptor per partition (`docker logs doceditor-redpanda-dev`: *Refusing to
/// create 1 partitions as total partition count 205 would exceed FD limit
/// 204*), so the cause is a count, and the count belongs in the message.
fn refused_topic(topic: &str, error: &str, live_topics: usize) -> String {
    format!(
        "broker refused to create {topic}: {error}. The broker holds {live_topics} topic(s); \
         this dev container allocates one file descriptor per partition and refuses every \
         create once the total would exceed its FD limit (204 under --smp 1 --overprovisioned), \
         with exactly this error. That is broker saturation, not a code regression — the \
         CloudEvents envelope is unchanged. Runs of this file prune their own topics; to empty \
         one saturated by older runs: {PRUNE_TOPICS}"
    )
}

/// A topic that deletes itself.
///
/// Why a guard rather than a line at the end of each test: a test that fails
/// never reaches its last line, and the runs that fail are exactly the ones a
/// reviewer repeats. `Drop` runs on the unwinding path too, so a red run costs
/// the broker nothing.
struct RoundTripTopic {
    brokers: String,
    name: String,
}

impl RoundTripTopic {
    async fn create(brokers: &str, label: &str) -> Self {
        let admin = admin_client(brokers);

        let pruned = prune_stale_topics(&admin, now_secs()).await;
        if pruned > 0 {
            eprintln!("swept {pruned} topic(s) left behind by earlier runs of this file");
        }

        let name = topic_name(label, now_secs());
        let results = admin
            .create_topics(
                &[NewTopic::new(&name, 1, TopicReplication::Fixed(1))],
                // Shorter than librdkafka's 60s default on purpose: the only way
                // this call times out is that there is no broker, and in that
                // case the useful behaviour is to say so quickly rather than to
                // hold CI for a minute per test while producing the same message.
                &admin_options(),
            )
            .await
            .unwrap_or_else(|e| panic!("{}", unreachable_broker(brokers, &name, &e.to_string())));

        for result in results {
            if let Err((refused, e)) = result {
                let live = topic_names(&admin).len();
                panic!("{}", refused_topic(&refused, &e.to_string(), live));
            }
        }

        Self {
            brokers: brokers.to_string(),
            name,
        }
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for RoundTripTopic {
    fn drop(&mut self) {
        let brokers = std::mem::take(&mut self.brokers);
        let name = std::mem::take(&mut self.name);
        let deleted = name.clone();

        // `drop` is synchronous and runs on a tokio worker here, where
        // `block_on` panics. A fresh thread with its own current-thread runtime
        // is the one way to finish an async deletion from inside a destructor,
        // and joining it makes the teardown deterministic rather than hopeful.
        let outcome = std::thread::spawn(move || -> bool {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return false;
            };
            runtime.block_on(async move {
                match admin_client(&brokers)
                    .delete_topics(&[deleted.as_str()], &admin_options())
                    .await
                {
                    Ok(results) => results.into_iter().all(|r| r.is_ok()),
                    Err(_) => false,
                }
            })
        })
        .join();

        // Never panic out of a destructor: unwinding from a drop that is itself
        // running during an unwind aborts the process, which would replace a
        // readable assertion failure with a bare SIGABRT. A topic that survives
        // is swept by the next run — that is what the sweep is for.
        if !matches!(outcome, Ok(true)) {
            eprintln!("could not delete {name}; the next run's sweep will take it");
        }
    }
}

/// What a machine without a broker is told, and what it can do about it.
///
/// The message ends up in `~/dev/ops/outputs/doceditor-test.log`, which is the
/// only thing the pipeline's triage reads. On 2026-09-13 it named the address
/// and pointed at `tests/common/mod.rs`, which cost a whole dev turn to open
/// and re-derive the one command below. So the command travels *with* the
/// failure.
#[test]
fn the_unreachable_broker_message_hands_the_reader_the_remedy() {
    let message = unreachable_broker(
        "127.0.0.1:19092",
        "doceditor-roundtrip-created-42",
        "Admin operation error: OperationTimedOut",
    );

    for needle in [
        "127.0.0.1:19092",
        "doceditor-roundtrip-created-42",
        "OperationTimedOut",
        "does not skip",
        "docker run",
        "redpandadata/redpanda",
    ] {
        assert!(
            message.contains(needle),
            "the failure a triage will read does not mention {needle:?}: {message}"
        );
    }

    // The remedy must start a broker at the address the harness actually dials
    // when `REDPANDA_BROKERS` is unset. A command that advertises another port
    // reads as help and leaves the suite just as red.
    assert!(
        START_A_BROKER.contains(&format!(
            "--advertise-kafka-addr PLAINTEXT://{}",
            common::FALLBACK_BROKERS
        )),
        "the suggested command does not advertise {}: {START_A_BROKER}",
        common::FALLBACK_BROKERS
    );
}

/// The sweep's blast radius, asked of the names it will actually see.
///
/// This is the test that makes an automatic `delete_topics` safe to ship: the
/// standing broker is shared with the service's own topic and with the
/// cluster's internals, and the sweep runs unattended on every test run.
#[test]
fn the_sweep_matches_only_this_files_own_topics_and_only_once_they_are_old() {
    let now = 1_757_838_000;

    // A topic this run just created is never swept — including by the two other
    // tests in this file, which start within milliseconds of it.
    assert!(!topic_is_stale(&topic_name("created", now), now));
    assert!(!topic_is_stale(&topic_name("lifecycle", now - 60), now));
    assert!(!topic_is_stale(
        &topic_name("silent", now - STALE_AFTER_SECS + 1),
        now
    ));

    // A run that died an hour ago left its topic behind and nothing else will
    // ever delete it.
    assert!(topic_is_stale(
        &topic_name("created", now - STALE_AFTER_SECS),
        now
    ));
    assert!(topic_is_stale(&topic_name("created", now - 86_400), now));

    // A name minted before this file stamped its topics — 186 of these had
    // accumulated on the standing broker by 2026-09-14.
    assert!(topic_is_stale(
        "doceditor-roundtrip-created-d08bc794-4ed9-4d2e-a7a3-a9bfe420e45d",
        now
    ));

    // Nothing else on a shared broker may ever be matched, whatever its age.
    for untouchable in [
        "editor.events",
        "ods.editor.events",
        "editor-events",
        "editor-events-dlq",
        "__consumer_offsets",
        "_schemas",
        "docstore-roundtrip-1757838000-created-x",
        "doceditor-events",
        "probe-alive-fa90f459-dc70-479c-8375-2889b0de702b",
    ] {
        assert!(
            !topic_is_stale(untouchable, now),
            "the sweep would have deleted {untouchable:?}, which this file did not create"
        );
    }

    // A clock that went backwards (a container resumed, a host resynced) must
    // not make a live topic look ancient.
    assert!(!topic_is_stale(&topic_name("created", now + 600), now));
}

/// What the triage reads when the broker refuses, and why it is not the diff.
///
/// On 2026-09-14 this exact refusal cost a review cycle: `InvalidPartitions`
/// read alone is indistinguishable from a code regression, and the cause was a
/// count of leftover topics nobody was looking at.
#[test]
fn the_refusal_a_triage_will_read_names_the_ceiling_and_hands_the_prune_command() {
    let message = refused_topic(
        "doceditor-roundtrip-1757838000-created-42",
        "Broker: Invalid number of partitions",
        204,
    );

    for needle in [
        "doceditor-roundtrip-1757838000-created-42",
        "Invalid number of partitions",
        "204 topic(s)",
        "file descriptor",
        "not a code regression",
        "rpk topic delete",
    ] {
        assert!(
            message.contains(needle),
            "the failure a triage will read does not mention {needle:?}: {message}"
        );
    }

    // Same rule as START_A_BROKER: a remedy aimed at an address nobody dials is
    // help that leaves the suite just as red.
    assert!(
        PRUNE_TOPICS.contains(common::FALLBACK_BROKERS),
        "the prune command does not address {}: {PRUNE_TOPICS}",
        common::FALLBACK_BROKERS
    );
    // And it must not be able to take the service's own topic with it.
    assert!(
        PRUNE_TOPICS.contains(&format!("/^{TOPIC_PREFIX}/")),
        "the prune command does not restrict itself to {TOPIC_PREFIX}: {PRUNE_TOPICS}"
    );
}

/// The leak this file used to be, measured against the broker itself.
///
/// Before 2026-09-14 every run created three topics and deleted none. The
/// standing broker is deliberately never recycled (it is the ADLC pipeline's
/// only bus), it allocates one file descriptor per partition, and it refuses
/// every create past 204 — so ~65 runs of this file were enough to turn the
/// whole suite red for a reason that is not in any diff. 186 leftovers had to
/// be pruned by hand on 2026-09-14.
#[tokio::test]
async fn the_topic_a_test_creates_is_gone_once_its_guard_drops() {
    let brokers = common::broker_addr();
    let admin = admin_client(&brokers);

    let name = {
        let topic = RoundTripTopic::create(&brokers, "teardown").await;
        let name = topic.name().to_string();
        assert!(
            topic_names(&admin).contains(&name),
            "{name} was not created, so its deletion below would prove nothing"
        );
        name
    };

    // The delete is acknowledged by the controller before the metadata every
    // client sees has caught up; poll rather than assume either way.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut still_there = true;
    while Instant::now() < deadline {
        if !topic_names(&admin).contains(&name) {
            still_there = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    assert!(
        !still_there,
        "{name} survived its guard: every run of this file would leave three of these on the \
         standing broker, which refuses every create once they reach its FD ceiling"
    );
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
    let topic = RoundTripTopic::create(&brokers, "created").await;
    let topic = topic.name();

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

    let producer = RedpandaProducer::new(&brokers, topic).expect("could not build the producer");
    producer
        .publish(event.clone())
        .expect("the producer refused to enqueue the event");

    let received = consume(&brokers, topic, 1);

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
    let topic = RoundTripTopic::create(&brokers, "lifecycle").await;
    let topic = topic.name();

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

    let producer = RedpandaProducer::new(&brokers, topic).expect("could not build the producer");
    for event in events {
        producer
            .publish(event)
            .expect("the producer refused to enqueue");
    }

    let received = consume(&brokers, topic, expected_types.len());

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
    let topic = RoundTripTopic::create(&brokers, "silent").await;
    let topic = topic.name();

    // A short window on purpose: this test asserts an *absence*, so its cost is
    // pure waiting. Long enough that a delivery in flight would be seen (the
    // two tests above routinely come back in well under a second), short enough
    // that it does not dominate the suite.
    let received = consume_for(&brokers, topic, 1, Duration::from_secs(5));

    assert!(
        received.is_empty(),
        "{} message(s) came back from a topic nothing was published to — \
         the round-trip assertions in this file prove nothing",
        received.len()
    );
}
