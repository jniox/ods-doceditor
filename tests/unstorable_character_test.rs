//! The one character this service cannot store, refused where it arrives.
//!
//! `U+0000` is a perfectly ordinary character in JSON (`"\u0000"`) and in a
//! query string (`%00`), and it is the one character PostgreSQL refuses in
//! `text` and in `jsonb` — `22021 invalid byte sequence for encoding "UTF8":
//! 0x00` and `22P05 unsupported Unicode escape sequence`. Nothing in this
//! service looked for it, so five fields carried it straight to the database
//! and the database wrote the reply. Measured on the running binary before this
//! file existed (2026-09-15, port 18087, HS256 token, `EVENT_BUS=pubsub`
//! against a local stand-in):
//!
//! ```text
//! POST  {"title":"a\u0000b"}                   -> 500 {"error":"internal_error"}
//! POST  {"title":"…","content":"hello\u0000…"} -> 500 {"error":"internal_error"}
//! POST  {"title":"…","metadata":{"k":"a\u0000b"}} -> 500 {"error":"internal_error"}
//! GET   /api/v1/documents?search=%00           -> 500 {"error":"internal_error"}
//! GET   /api/v1/documents?status=draft         -> 200   (the control)
//! ```
//!
//! and in the service log, four times: `Database error: error returned from
//! database: invalid byte sequence for encoding "UTF8"` / `unsupported Unicode
//! escape sequence`.
//!
//! `500 internal_error` is the wrong answer three times over. It says *our
//! fault, try again* about a request that can never succeed, so a client with a
//! retry policy retries for ever; it carries no field name, so nobody can act
//! on it; and it raises an ERROR log line — a page, on a service whose 500s are
//! supposed to be rare — for an input a caller chose. The contract already has
//! the right answer for this: `422` is "the request was well-formed but a value
//! is unacceptable", and a query parameter this service will not accept is a
//! `400` (`?status=published%20`, `?page=abc`).
//!
//! Same shape as the byte/character bound of `tests/text_bounds_test.rs`: a
//! constraint that lives in the **column** rather than in the code, refusing
//! what the boundary happily accepted. The cure is the one this repository
//! keeps: the rule is named once — `domain::text::nul_at` — and every string a
//! caller can put in the database crosses it.
//!
//! The refusal is exactly one character wide. `U+0001`, its neighbour, is as
//! unprintable as `U+0000` and PostgreSQL stores it without complaint, so it is
//! still accepted here and still returned verbatim: this is a bound, not a
//! sanitiser, and the contract's promise that a body is "neither parsed,
//! sanitised nor escaped" stands.

mod common;

use actix_web::{test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, payload, versions};
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

const BODY_CEILING: usize = 1024 * 1024;

/// The character under test, written once.
const NUL: char = '\u{0}';
/// Its neighbour, which PostgreSQL stores happily — the control that keeps the
/// refusal one character wide instead of "unprintable characters".
const NEIGHBOUR: char = '\u{1}';

macro_rules! app {
    ($pool:expr, $jwt:expr) => {{
        let svc = DocumentService::new($pool.clone(), Arc::new(InMemoryProducer::new()))
            .with_max_content_bytes(BODY_CEILING);
        test::init_service(
            App::new()
                .app_data(web::Data::new($pool.clone()))
                .app_data(web::Data::new(svc))
                .app_data(web::Data::new($jwt))
                .configure(payload::limits(BODY_CEILING))
                .route(
                    "/api/v1/documents",
                    web::post().to(documents::create_document),
                )
                .route(
                    "/api/v1/documents",
                    web::get().to(documents::list_documents),
                )
                .route(
                    "/api/v1/documents/{id}",
                    web::get().to(documents::get_document),
                )
                .route(
                    "/api/v1/documents/{id}",
                    web::patch().to(documents::update_document),
                )
                .route(
                    "/api/v1/documents/{id}/versions",
                    web::post().to(versions::create_version),
                ),
        )
        .await
    }};
}

macro_rules! post_document {
    ($app:expr, $token:expr, $body:expr) => {{
        let req = test::TestRequest::post()
            .uri("/api/v1/documents")
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .set_json($body)
            .to_request();
        test::call_service(&$app, req).await
    }};
}

macro_rules! get {
    ($app:expr, $token:expr, $uri:expr) => {{
        let req = test::TestRequest::get()
            .uri($uri)
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .to_request();
        test::call_service(&$app, req).await
    }};
}

/// The error body every refusal of this service carries (AC-031).
async fn error_body(
    resp: actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
) -> serde_json::Value {
    test::read_body_json(resp).await
}

/// Create an ordinary document and return its id.
macro_rules! a_document {
    ($app:expr, $token:expr) => {{
        let resp = post_document!(
            $app,
            $token,
            serde_json::json!({ "title": "Document de travail", "content": "<p>x</p>" })
        );
        assert_eq!(resp.status(), 201, "the fixture document was not created");
        let created: serde_json::Value = test::read_body_json(resp).await;
        created["id"].as_str().unwrap().to_string()
    }};
}

/// **The premise**, asked of PostgreSQL rather than asserted in prose: this is
/// why the boundary refuses, and if it ever stops being true the rest of this
/// file is over-zealous rather than protective.
#[actix_web::test]
async fn postgresql_cannot_store_this_character_in_text_or_in_jsonb() {
    let pool = setup_test_pool().await;

    let as_text = sqlx::query_scalar::<_, String>("SELECT $1::text")
        .bind(format!("a{NUL}b"))
        .fetch_one(&pool)
        .await;
    let err =
        as_text.expect_err("PostgreSQL accepted U+0000 in text — this file's premise is gone");
    let code = err
        .as_database_error()
        .and_then(|e| e.code())
        .map(|c| c.to_string())
        .unwrap_or_default();
    assert_eq!(code, "22021", "unexpected refusal for text: {err}");

    let as_jsonb = sqlx::query_scalar::<_, serde_json::Value>("SELECT $1::jsonb")
        .bind(serde_json::json!({ "k": format!("a{NUL}b") }))
        .fetch_one(&pool)
        .await;
    let err =
        as_jsonb.expect_err("PostgreSQL accepted U+0000 in jsonb — this file's premise is gone");
    let code = err
        .as_database_error()
        .and_then(|e| e.code())
        .map(|c| c.to_string())
        .unwrap_or_default();
    assert_eq!(code, "22P05", "unexpected refusal for jsonb: {err}");

    // And the neighbour, which is why the refusal is one character wide.
    let neighbour = sqlx::query_scalar::<_, String>("SELECT $1::text")
        .bind(format!("a{NEIGHBOUR}b"))
        .fetch_one(&pool)
        .await
        .expect("U+0001 is storable");
    assert_eq!(neighbour, format!("a{NEIGHBOUR}b"));
}

#[actix_web::test]
async fn a_title_carrying_it_is_refused_by_this_service_and_not_by_the_column() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let resp = post_document!(
        app,
        token,
        serde_json::json!({ "title": format!("a{NUL}b") })
    );
    assert_eq!(resp.status(), 422, "a title carrying U+0000");
    let body = error_body(resp).await;
    assert_eq!(body["error"], "validation_error");
    let message = body["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("title"),
        "the refusal must name the field: {message}"
    );
}

#[actix_web::test]
async fn a_body_carrying_it_is_refused_naming_the_field() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let resp = post_document!(
        app,
        token,
        serde_json::json!({ "title": "Contrat", "content": format!("hello{NUL}world") })
    );
    assert_eq!(resp.status(), 422, "a body carrying U+0000");
    let body = error_body(resp).await;
    assert_eq!(body["error"], "validation_error");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("content"),
        "the refusal must name the field: {}",
        body["message"]
    );

    // The rename path is the other half: lot 10's defect was that creation and
    // update did not apply the same rule to the same field.
    let id = a_document!(app, token);
    let req = test::TestRequest::patch()
        .uri(&format!("/api/v1/documents/{id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "content": format!("hello{NUL}world") }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 422, "PATCH content carrying U+0000");

    let req = test::TestRequest::patch()
        .uri(&format!("/api/v1/documents/{id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "title": format!("a{NUL}b") }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 422, "PATCH title carrying U+0000");
}

/// Metadata is `jsonb`, so the character is refused wherever it sits — a value,
/// a value nested in an array or an object, **and a key at any depth**. Nested
/// keys are outside the `^[a-z][a-z0-9_]{0,63}$` rule (which the contract
/// states for the object's own keys), so they are the one place nothing looked.
#[actix_web::test]
async fn metadata_carrying_it_is_refused_at_every_depth_in_keys_as_well_as_values() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let shapes = [
        serde_json::json!({ "kind": format!("a{NUL}b") }),
        serde_json::json!({ "kind": { "inner": format!("a{NUL}b") } }),
        serde_json::json!({ "kind": [1, format!("a{NUL}b")] }),
        serde_json::json!({ "kind": { format!("nested{NUL}key"): "v" } }),
    ];
    for shape in shapes {
        let resp = post_document!(
            app,
            token,
            serde_json::json!({ "title": "Contrat", "metadata": shape })
        );
        assert_eq!(resp.status(), 422, "metadata {shape} carrying U+0000");
        let body = error_body(resp).await;
        assert_eq!(body["error"], "validation_error");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("metadata"),
            "the refusal must name the field: {}",
            body["message"]
        );
    }
}

#[actix_web::test]
async fn a_snapshot_comment_carrying_it_is_refused() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());
    let id = a_document!(app, token);

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/documents/{id}/versions"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "comment": format!("relu{NUL}par moi") }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 422, "a comment carrying U+0000");
    let body = error_body(resp).await;
    assert_eq!(body["error"], "validation_error");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("comment"),
        "the refusal must name the field: {}",
        body["message"]
    );
}

/// The query string is the fifth place, and its refusal is the query string's:
/// `400`, like `?status=published%20` and `?page=abc`, not `422`.
#[actix_web::test]
async fn a_search_term_carrying_it_is_a_bad_request_and_not_a_500() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let resp = get!(app, token, "/api/v1/documents?search=%00");
    assert_eq!(resp.status(), 400, "?search=%00");
    let body = error_body(resp).await;
    assert_eq!(body["error"], "bad_request");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("search"),
        "the refusal must name the parameter: {}",
        body["message"]
    );

    // Buried in the middle of an otherwise ordinary term, too.
    let resp = get!(app, token, "/api/v1/documents?search=clause%00resolutoire");
    assert_eq!(resp.status(), 400, "?search=clause%00resolutoire");
}

/// Non-vacuity, and the width of the rule: the neighbouring control character
/// is still accepted, still stored and still returned byte-for-byte. A body is
/// "neither parsed, sanitised nor escaped" (ADR-001) — this change refuses one
/// character, it does not clean anything.
#[actix_web::test]
async fn the_neighbouring_control_character_is_still_stored_verbatim() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let content = format!("hello{NEIGHBOUR}world");
    let title = format!("a{NEIGHBOUR}b");
    let resp = post_document!(
        app,
        token,
        serde_json::json!({
            "title": title,
            "content": content,
            "metadata": { "kind": format!("x{NEIGHBOUR}y") },
        })
    );
    assert_eq!(
        resp.status(),
        201,
        "U+0001 is storable and must not be refused"
    );
    let created: serde_json::Value = test::read_body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();

    let resp = get!(app, token, &format!("/api/v1/documents/{id}"));
    assert_eq!(resp.status(), 200);
    let doc: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(doc["content"].as_str().unwrap(), content);
    assert_eq!(doc["title"].as_str().unwrap(), title);
    assert_eq!(
        doc["metadata"]["kind"].as_str().unwrap(),
        format!("x{NEIGHBOUR}y")
    );

    // And an ordinary search still searches.
    let resp = get!(app, token, "/api/v1/documents?search=hello");
    assert_eq!(resp.status(), 200, "an ordinary search term still works");
}
