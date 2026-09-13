//! AC-013 — `X-Correlation-Id` and `X-Source-Service`.
//!
//! The platform rule requires these headers to be handled so a call can be
//! followed across services. Before this batch neither string appeared anywhere
//! in `src/` or `tests/`: a request crossing doceditor left no trace that could
//! be joined to the caller's logs, and the events it produced carried no way
//! back to the request that caused them.

mod common;

use actix_web::{middleware::from_fn, test, web, App, HttpResponse};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::middleware::{correlate, RequestContext};
use ods_doceditor::api::{documents, health};
use ods_doceditor::correlation::{CORRELATION_ID_HEADER, SOURCE_SERVICE_HEADER};
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

/// A correlation id supplied by the caller is the one that comes back.
#[actix_web::test]
async fn test_caller_supplied_correlation_id_is_echoed() {
    let app = test::init_service(
        App::new()
            .wrap(from_fn(correlate))
            .route("/health", web::get().to(health::health)),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/health")
        .insert_header((CORRELATION_ID_HEADER, "trace-from-docsign-42"))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(
        resp.headers().get(CORRELATION_ID_HEADER).unwrap(),
        "trace-from-docsign-42"
    );
}

/// With no caller id, one is minted rather than the trace being dropped.
#[actix_web::test]
async fn test_missing_correlation_id_is_generated() {
    let app = test::init_service(
        App::new()
            .wrap(from_fn(correlate))
            .route("/health", web::get().to(health::health)),
    )
    .await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;

    let generated = resp
        .headers()
        .get(CORRELATION_ID_HEADER)
        .expect("a correlation id must always come back")
        .to_str()
        .unwrap()
        .to_string();

    assert!(
        Uuid::parse_str(&generated).is_ok(),
        "a generated correlation id should be a uuid, got {generated}"
    );
}

/// Both headers reach the handler through the `RequestContext` extractor.
#[actix_web::test]
async fn test_request_context_exposes_both_headers_to_handlers() {
    async fn echo(ctx: RequestContext) -> HttpResponse {
        HttpResponse::Ok().json(serde_json::json!({
            "correlation_id": ctx.correlation_id,
            "source_service": ctx.source_service,
        }))
    }

    let app = test::init_service(
        App::new()
            .wrap(from_fn(correlate))
            .route("/echo", web::get().to(echo)),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/echo")
        .insert_header((CORRELATION_ID_HEADER, "corr-1"))
        .insert_header((SOURCE_SERVICE_HEADER, "docsign"))
        .to_request();
    let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;

    assert_eq!(body["correlation_id"], "corr-1");
    assert_eq!(body["source_service"], "docsign");

    // Absent source service is absent, not an empty string pretending to be one.
    let req = test::TestRequest::get()
        .uri("/echo")
        .insert_header((CORRELATION_ID_HEADER, "corr-2"))
        .to_request();
    let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    assert!(body["source_service"].is_null());
}

/// The events a request produces carry the correlation id of that request.
/// This is the point of the whole mechanism: joining an HTTP call to what it
/// caused downstream.
#[actix_web::test]
async fn test_events_carry_the_correlation_id_of_their_request() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let app = test::init_service(
        App::new()
            .wrap(from_fn(correlate))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents",
                web::post().to(documents::create_document),
            ),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .insert_header((CORRELATION_ID_HEADER, "corr-traceable"))
        .set_json(serde_json::json!({ "title": "Traced", "content": "body" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 201);

    let events = producer.get_events();
    assert!(!events.is_empty());
    for event in events {
        assert_eq!(
            event.correlationid.as_deref(),
            Some("corr-traceable"),
            "event {} lost its correlation id",
            event.event_type
        );
    }
}

/// Outside a request there is simply no correlation id — the mechanism must
/// not invent one, or a background job would look like someone's HTTP call.
#[actix_web::test]
async fn test_events_produced_outside_a_request_have_no_correlation_id() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());

    svc.create_document(
        Uuid::new_v4(),
        "Hors requête",
        Uuid::new_v4(),
        serde_json::json!({}),
        None,
        None,
    )
    .await
    .unwrap();

    for event in producer.get_events() {
        assert!(event.correlationid.is_none());
    }
}
