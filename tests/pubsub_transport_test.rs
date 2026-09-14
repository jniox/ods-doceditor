//! The transport, measured on the wire rather than asserted about the code.
//!
//! Decision **HR-20260914-007**, option A, taken 2026-09-14T11:07:01 by
//! it@orbusdigital.com, enactor *"dev, dans le dépôt doceditor"*: **port the
//! producer to Pub/Sub**. The asymmetry that made the question decidable is
//! that the deployment had already chosen — re-measured first-hand for this
//! batch on the live revision `doceditor-00003-vkq`:
//!
//! ```text
//! EVENT_BUS        = pubsub
//! PUBSUB_TOPIC     = editor-events          (exists: gcloud pubsub topics list)
//! PUBSUB_TOPIC_DLQ = editor-events-dlq      (exists)
//! GCP_PROJECT_ID   = orbus-ods-staging
//! serviceAccount   = runtime-cloud-run@orbus-ods-staging.iam.gserviceaccount.com
//!                    -> roles/pubsub.publisher  (project IAM, already granted)
//! ```
//!
//! Four variables no line of `src/config.rs` read. The code now reads them.
//!
//! **Why this file talks to a socket.** `tests/events_roundtrip.rs` exists
//! because "the producer returned `Ok`" is not evidence — this service published
//! four months of CloudEvents into a `NoopProducer` and nothing failed (ADR-002).
//! The same standard applies to the new transport: everything below is asserted
//! against a real HTTP server that records what the producer actually sent,
//! including the token exchange. A unit test of the mapping alone would repeat
//! lot 9's mistake of measuring a boundary from a bench the boundary does not
//! exist in.

use std::sync::{Arc, Mutex};

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use base64::Engine;
use ods_doceditor::config::{select_event_bus, EventBus, EventBusEnv};
use ods_doceditor::events::producer::{CloudEvent, EventProducer};
use ods_doceditor::events::pubsub::{pubsub_message, PubSubAuth, PubSubEndpoints, PubSubProducer};
use uuid::Uuid;

/// One request the fake Pub/Sub endpoint received, kept whole.
#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    authorization: Option<String>,
    metadata_flavor: Option<String>,
    body: serde_json::Value,
}

#[derive(Clone)]
struct FakePubSub {
    requests: Arc<Mutex<Vec<Recorded>>>,
    /// The status the publish route answers with, so a refusal can be measured
    /// as well as an acceptance.
    publish_status: u16,
    base_url: String,
}

impl FakePubSub {
    fn start(publish_status: u16) -> Self {
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let recorder = requests.clone();

        std::thread::spawn(move || {
            let system = actix_web::rt::System::new();
            system.block_on(async move {
                let server = HttpServer::new(move || {
                    let recorder = recorder.clone();
                    App::new().default_service(web::to(
                        move |req: HttpRequest, body: web::Bytes| {
                            let recorder = recorder.clone();
                            async move {
                                let header = |name: &str| {
                                    req.headers()
                                        .get(name)
                                        .and_then(|v| v.to_str().ok())
                                        .map(str::to_string)
                                };
                                let path = req.uri().to_string();
                                recorder.lock().unwrap().push(Recorded {
                                    path: path.clone(),
                                    authorization: header("authorization"),
                                    metadata_flavor: header("metadata-flavor"),
                                    body: serde_json::from_slice(&body)
                                        .unwrap_or(serde_json::Value::Null),
                                });

                                if path.ends_with("/token") {
                                    return HttpResponse::Ok().json(serde_json::json!({
                                        "access_token": "fake-access-token",
                                        "expires_in": 3599,
                                        "token_type": "Bearer",
                                    }));
                                }
                                HttpResponse::build(
                                    actix_web::http::StatusCode::from_u16(publish_status).unwrap(),
                                )
                                .json(serde_json::json!({"messageIds": ["42"]}))
                            }
                        },
                    ))
                })
                .workers(1)
                .bind(("127.0.0.1", 0))
                .expect("the fake Pub/Sub endpoint must bind");
                let addr = server.addrs()[0];
                tx.send(addr).unwrap();
                server.run().await.unwrap();
            });
        });

        let addr = rx
            .recv()
            .expect("the fake endpoint must report its address");
        Self {
            requests,
            publish_status,
            base_url: format!("http://{addr}"),
        }
    }

    fn endpoints(&self) -> PubSubEndpoints {
        PubSubEndpoints {
            publish_base: self.base_url.clone(),
            auth: PubSubAuth::MetadataServer {
                base: self.base_url.clone(),
            },
        }
    }

    fn taken(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

fn sample_event() -> CloudEvent {
    CloudEvent::document_created(
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
        "Contrat de prestation",
        Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
    )
    .with_correlation_id("corr-42")
}

/// The whole publish, end to end: token exchange, URL, envelope.
///
/// This is the assertion the previous four months lacked — not "publish
/// returned Ok" but "the bus received exactly this".
#[actix_web::test]
async fn an_event_reaches_the_bus_as_a_cloudevent_in_binary_content_mode() {
    let fake = FakePubSub::start(200);
    let producer = PubSubProducer::new("orbus-ods-staging", "editor-events", fake.endpoints())
        .expect("the producer must build");

    producer
        .publish_once(&sample_event())
        .await
        .expect("the fake bus accepted the event");

    let taken = fake.taken();
    assert_eq!(
        taken.len(),
        2,
        "one token exchange then one publish; got {taken:#?}"
    );

    // 1. The token came from the instance metadata server, asked for the way
    //    that server requires — the header is not optional, it is what proves
    //    the request did not come from a browser.
    let token = &taken[0];
    assert!(
        token
            .path
            .ends_with("/computeMetadata/v1/instance/service-accounts/default/token"),
        "the access token must be taken from the instance metadata server; got {}",
        token.path
    );
    assert_eq!(token.metadata_flavor.as_deref(), Some("Google"));

    // 2. The publish went to the project and topic configuration names, and
    //    nowhere else. Publishing to the wrong name is silent (ADR-002), so the
    //    URL is worth asserting in full.
    let publish = &taken[1];
    assert_eq!(
        publish.path, "/v1/projects/orbus-ods-staging/topics/editor-events:publish",
        "the publish URL must name the configured project and topic"
    );
    assert_eq!(
        publish.authorization.as_deref(),
        Some("Bearer fake-access-token"),
        "the token the metadata server handed over must be the one presented"
    );

    // 3. The envelope: CloudEvents binary content mode, transposed onto Pub/Sub
    //    message attributes.
    let message = &publish.body["messages"][0];
    let attributes = message["attributes"]
        .as_object()
        .expect("a Pub/Sub message carries its CloudEvents attributes");
    for (name, expected) in [
        ("ce-specversion", "1.0"),
        ("ce-type", "com.ods.editor.document.created"),
        ("ce-source", "/editor"),
        ("ce-tenantid", "11111111-1111-1111-1111-111111111111"),
        ("ce-correlationid", "corr-42"),
        ("content-type", "application/json"),
    ] {
        assert_eq!(
            attributes.get(name).and_then(|v| v.as_str()),
            Some(expected),
            "attribute {name}"
        );
    }
    assert!(attributes.contains_key("ce-id"), "every event has an id");
    assert!(attributes.contains_key("ce-time"), "every event has a time");

    // 4. The body is the event data, base64 as Pub/Sub requires — and it is the
    //    data alone, because the attributes carry the envelope.
    let data = base64::engine::general_purpose::STANDARD
        .decode(message["data"].as_str().expect("data is a base64 string"))
        .expect("data must decode");
    let data: serde_json::Value = serde_json::from_slice(&data).expect("data is the event's JSON");
    assert_eq!(data["title"], "Contrat de prestation");
    assert_eq!(data["document_id"], "22222222-2222-2222-2222-222222222222");

    // 5. Ordering key = document id, the transposition of the Kafka partition
    //    key, so a document's history keeps its order (AC-012).
    assert_eq!(
        message["orderingKey"].as_str(),
        Some("22222222-2222-2222-2222-222222222222")
    );
}

/// A bus that refuses is an error, not a shrug.
///
/// The failure this repository has actually lived through is the silent one, so
/// the refusal path is the one worth a test: a 403 (the shape an ungranted
/// `roles/pubsub.publisher` takes) must come back as `Err`, carrying the status
/// so the log names what happened.
#[actix_web::test]
async fn a_bus_that_refuses_the_event_produces_an_error_naming_the_refusal() {
    let fake = FakePubSub::start(403);
    let producer = PubSubProducer::new("orbus-ods-staging", "editor-events", fake.endpoints())
        .expect("the producer must build");

    let refusal = producer
        .publish_once(&sample_event())
        .await
        .expect_err("a 403 from the bus is a failure to publish");
    assert!(
        refusal.contains("403"),
        "the refusal must name the status the bus answered with; got {refusal:?}"
    );
    assert_eq!(fake.publish_status, 403);
}

/// An unreachable bus is an error too — not a hang, and not an `Ok`.
#[actix_web::test]
async fn an_unreachable_bus_is_an_error_rather_than_a_silent_success() {
    // Port 1 on loopback: nothing listens, and the refusal is immediate.
    let producer = PubSubProducer::new(
        "orbus-ods-staging",
        "editor-events",
        PubSubEndpoints {
            publish_base: "http://127.0.0.1:1".to_string(),
            auth: PubSubAuth::Emulator,
        },
    )
    .expect("the producer must build");

    producer
        .publish_once(&sample_event())
        .await
        .expect_err("an unreachable bus cannot be reported as a published event");
}

/// The emulator path presents no credentials, and must not try to.
///
/// `PUBSUB_EMULATOR_HOST` is Google's own knob and it is unauthenticated; asking
/// a metadata server that is not there would turn local development into a
/// 30-second hang per event.
#[actix_web::test]
async fn the_emulator_path_publishes_without_asking_for_a_token() {
    let fake = FakePubSub::start(200);
    let producer = PubSubProducer::new(
        "orbus-ods-staging",
        "editor-events",
        PubSubEndpoints {
            publish_base: fake.base_url.clone(),
            auth: PubSubAuth::Emulator,
        },
    )
    .expect("the producer must build");

    producer.publish_once(&sample_event()).await.unwrap();

    let taken = fake.taken();
    assert_eq!(taken.len(), 1, "no token exchange against an emulator");
    assert_eq!(taken[0].authorization, None);
}

/// The token is fetched once and reused, so a burst of events is not a burst of
/// metadata-server calls.
#[actix_web::test]
async fn the_access_token_is_reused_across_events() {
    let fake = FakePubSub::start(200);
    let producer = PubSubProducer::new("orbus-ods-staging", "editor-events", fake.endpoints())
        .expect("the producer must build");

    for _ in 0..3 {
        producer.publish_once(&sample_event()).await.unwrap();
    }

    let token_calls = fake
        .taken()
        .iter()
        .filter(|r| r.path.ends_with("/token"))
        .count();
    assert_eq!(
        token_calls, 1,
        "the access token must be cached until it expires"
    );
}

/// `EventProducer::publish` — the trait the service calls — reaches the bus too.
///
/// It spawns, because publishing must never hold an HTTP request hostage to bus
/// latency (the same rule the Kafka producer follows). What is asserted here is
/// that the spawn actually happens and lands.
#[actix_web::test]
async fn the_trait_method_publishes_through_the_same_path() {
    let fake = FakePubSub::start(200);
    let producer = PubSubProducer::new("orbus-ods-staging", "editor-events", fake.endpoints())
        .expect("the producer must build");
    assert_eq!(producer.name(), "pubsub");

    producer.publish(sample_event()).expect("enqueued");

    for _ in 0..100 {
        if fake.taken().iter().any(|r| r.path.ends_with(":publish")) {
            return;
        }
        actix_web::rt::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "the spawned publish never reached the bus: {:#?}",
        fake.taken()
    );
}

/// The mapping, stated once more from the outside: the Pub/Sub attributes and
/// the Kafka headers carry the same CloudEvents attributes.
///
/// Two bindings of one envelope drift the moment someone adds an attribute to
/// one of them. This is the same shape as `version_repo`'s two column lists and
/// `search_predicate`'s single expression: one source, two renderings.
#[test]
fn both_bindings_carry_the_same_cloudevents_attributes() {
    let event = sample_event();
    let kafka = ods_doceditor::events::producer::kafka_record(&event);
    let pubsub = pubsub_message(&event);

    let mut from_kafka: Vec<String> = kafka
        .headers
        .iter()
        .map(|(k, _)| k.replace("ce_", "ce-"))
        .collect();
    let mut from_pubsub: Vec<String> = pubsub.attributes.iter().map(|(k, _)| k.clone()).collect();
    from_kafka.sort();
    from_pubsub.sort();
    assert_eq!(
        from_kafka, from_pubsub,
        "the Kafka and Pub/Sub bindings must carry the same envelope"
    );
    assert_eq!(kafka.key, pubsub.ordering_key, "same partitioning value");
}

/// And the selection: what the live revision sets is what selects Pub/Sub.
#[test]
fn the_live_revisions_environment_selects_the_pubsub_transport() {
    let selected = select_event_bus(EventBusEnv {
        event_bus: Some("pubsub"),
        redpanda_brokers: None,
        redpanda_topic: None,
        pubsub_topic: Some("editor-events"),
        gcp_project_id: Some("orbus-ods-staging"),
    })
    .expect("the live revision's environment must be honoured");

    assert_eq!(
        selected,
        EventBus::PubSub {
            project_id: "orbus-ods-staging".to_string(),
            topic: "editor-events".to_string(),
        }
    );
}
