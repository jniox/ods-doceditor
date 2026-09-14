//! The two ceilings a document has to pass — and the one that was never
//! reachable.
//!
//! `MAX_DOCUMENT_SIZE_MB` is documented, in `.env.example` and in the repo's
//! `CLAUDE.md`, as bounding *both* the HTTP payload and the document body.
//! Making them the same number is what breaks: the payload has to carry the
//! body **plus its JSON encoding** — the quotes, the `title`, the `metadata` —
//! so a body of exactly the documented maximum always makes a payload larger
//! than the maximum. Measured on the running binary at `MAX_DOCUMENT_SIZE_MB=1`
//! before this file existed:
//!
//! ```text
//! content = ceiling - 64   wire = 1048551 -> 201
//! content = ceiling        wire = 1048615 -> 413 text/plain
//! content = ceiling + 1    wire = 1048616 -> 413 text/plain
//! content = ceiling / 2, all '"'  wire = 1048615 -> 413 text/plain
//! ```
//!
//! Three things are wrong there and only one of them is the status code.
//!
//! 1. A document of exactly the documented size **cannot be stored**. The real
//!    maximum is the ceiling minus an envelope the caller cannot compute.
//! 2. The service's own refusal — `422 {"error":"validation_error","message":
//!    "Content exceeds the maximum document size of N bytes"}` — is
//!    unreachable from HTTP. `DocumentService::validate_content` can only fire
//!    on the template path, where the body does not come from the payload.
//! 3. The fourth line is the one that gives the game away: a body of **half**
//!    the ceiling is refused because it is made of quote characters. The
//!    ceiling is being applied to the JSON *encoding*, so the largest document
//!    a client may store depends on which characters are in it.
//!
//! Why no test caught it: the only test of the ceiling
//! (`content_test::test_content_over_the_configured_limit_is_rejected`) builds
//! an `App` with **no** `JsonConfig` at all and calls a service constructed
//! with `with_max_content_bytes(64)`. It exercises the layer that enforces,
//! from a vantage point where the boundary that refuses first does not exist.
//! Its doc-comment claims the ceiling is "enforced on the body, not only on the
//! HTTP payload"; in production it was enforced *only* on the HTTP payload.
//!
//! So every test here goes through the boundary, and the boundary is wired by
//! `api::payload::limits` — the same function `main.rs` calls. A bench that
//! builds its own limits is a bench that can disagree with production, which is
//! the whole defect.

mod common;

use actix_web::{test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::payload::payload_ceiling;
use ods_doceditor::api::{documents, payload};
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

/// A small ceiling, so the assertions are about the arithmetic and not about
/// moving megabytes. `main.rs` derives the same two numbers from
/// `MAX_DOCUMENT_SIZE_MB` through the same function.
const BODY_CEILING: usize = 4096;

macro_rules! app_with_limits {
    ($pool:expr, $jwt:expr) => {{
        let svc = DocumentService::new($pool.clone(), Arc::new(InMemoryProducer::new()))
            .with_max_content_bytes(BODY_CEILING);
        test::init_service(
            App::new()
                .app_data(web::Data::new($pool.clone()))
                .app_data(web::Data::new(svc))
                .app_data(web::Data::new($jwt))
                // Exactly what src/main.rs installs, from the same call.
                .configure(payload::limits(BODY_CEILING))
                .route(
                    "/api/v1/documents",
                    web::post().to(documents::create_document),
                )
                .route(
                    "/api/v1/documents/{id}",
                    web::get().to(documents::get_document),
                ),
        )
        .await
    }};
}

fn body_of(repeated: char, times: usize) -> serde_json::Value {
    serde_json::json!({ "title": "size probe", "content": repeated.to_string().repeat(times) })
}

/// The promise the variable's own name makes: a document of exactly
/// `MAX_DOCUMENT_SIZE_MB` is storable. It was not.
#[actix_web::test]
async fn a_body_of_exactly_the_documented_maximum_is_stored_whole() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app_with_limits!(pool, test_jwt_config());

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(body_of('x', BODY_CEILING))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        201,
        "a body of exactly {BODY_CEILING} bytes — the documented maximum — was refused"
    );

    // And stored whole, not truncated: the round trip is the only proof that
    // the ceiling admits the value it names.
    let created: serde_json::Value = test::read_body_json(resp).await;
    let req = test::TestRequest::get()
        .uri(&format!(
            "/api/v1/documents/{}",
            created["id"].as_str().unwrap()
        ))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let fetched: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(fetched["content"].as_str().unwrap().len(), BODY_CEILING);
}

/// One byte over, and the answer must come from the service — which knows the
/// body is the problem — rather than from the framework, which only knows the
/// request was long.
#[actix_web::test]
async fn a_body_over_the_maximum_is_refused_by_the_service_in_its_own_shape() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app_with_limits!(pool, test_jwt_config());

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(body_of('x', BODY_CEILING + 1))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        422,
        "one byte over the body ceiling must be the service's 422, not the \
         framework's 413 on the whole request"
    );

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["error"], "validation_error");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(&BODY_CEILING.to_string()),
        "the refusal must name the document ceiling so a client knows what to \
         shrink, got {body}"
    );
}

/// The line that showed the ceiling was on the wrong quantity: a body **half**
/// the ceiling, refused because JSON doubles every quote character.
#[actix_web::test]
async fn json_escaping_does_not_shrink_the_document_a_caller_may_store() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app_with_limits!(pool, test_jwt_config());

    // Every byte escapes to two, so this is the worst case for a body of text.
    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(body_of('"', BODY_CEILING))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        201,
        "a body within the ceiling was refused for how it encodes, not for how \
         big it is — the ceiling is on the document, not on the wire"
    );
}

/// The payload ceiling still exists, and when it is what refuses, it refuses in
/// the service's shape and names both numbers.
#[actix_web::test]
async fn a_payload_beyond_the_envelope_allowance_is_refused_in_the_services_shape() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app_with_limits!(pool, test_jwt_config());

    // Comfortably past `payload_ceiling(BODY_CEILING)`, so the request is
    // refused before it is read at all.
    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(body_of('"', payload_ceiling(BODY_CEILING)))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 413);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json",
        "every other refusal of this service is JSON; this one answered \
         text/plain, which no client parses"
    );

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["error"], "payload_too_large");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(&BODY_CEILING.to_string()),
        "the refusal must name the document ceiling, not only the payload one — \
         the caller configures documents, got {body}"
    );
}

/// The same boundary handles a body it cannot parse at all. Installing an error
/// handler makes this ours too, so it must stay in the service's shape.
#[actix_web::test]
async fn a_malformed_body_is_refused_in_the_services_shape() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app_with_limits!(pool, test_jwt_config());

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(r#"{"title": "unterminated"#)
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["error"], "bad_request");
}

/// Non-vacuity: an ordinary document is untouched by any of this.
#[actix_web::test]
async fn an_ordinary_document_still_goes_through() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app_with_limits!(pool, test_jwt_config());

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "Contrat de prestation",
            "content": "<h1>Article 1</h1><p>Les parties conviennent.</p>",
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
}
