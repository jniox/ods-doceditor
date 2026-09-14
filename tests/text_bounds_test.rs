//! The bounds on the fields a human types — counted in the unit the contract
//! names, and applied to the value that is actually stored.
//!
//! `docs/openapi.yaml` bounds three of them, and every one of the three is a
//! **character** count (JSON Schema `maxLength` counts characters, and so does
//! `VARCHAR(n)` in PostgreSQL, which is what migration 002 and 003 declare):
//!
//! | field | contract | column |
//! |---|---|---|
//! | `title` | `minLength: 1, maxLength: 500`, "Non-blank once trimmed" | `VARCHAR(500)` |
//! | `comment` | `maxLength: 500` | `VARCHAR(500)` |
//! | metadata string values | "at most 256 characters" | `jsonb` |
//!
//! The service counted **bytes**, and on the rename path it validated one value
//! and stored another. Measured on the running binary before this file existed
//! (`MAX_DOCUMENT_SIZE_MB` default, HS256 token, port 8097):
//!
//! ```text
//! POST  title = 500 × 'é'  (500 chars)      -> 422 "Title must be 1-500 characters, non-blank"
//! POST  largest accepted accented title     -> 250 characters
//! POST  largest accepted CJK title          -> 166 characters
//! POST  title = 500 × 'a'                   -> 201
//! PATCH title = ' ' + 500 × 'a'             -> 500 {"error":"internal_error"}
//! PATCH title = '   Contrat   '             -> 200, stored '   Contrat   '
//! POST  title = '   Contrat   '             -> 201, stored 'Contrat'
//! POST  comment = 400 × 'é'  (400 chars)    -> 422 "Comment must be at most 500 characters"
//! ```
//!
//! Four things are wrong there, and they are one thing.
//!
//! 1. **The documented maximum is unreachable in every alphabet but one.** The
//!    largest title a caller may store was 500, 250 or 166 characters depending
//!    on which characters were in it — a number no caller can compute, on a
//!    product whose own examples are French (`Contrat de prestation`) and whose
//!    market is OHADA/Senegal.
//! 2. **A legitimate rename answered `500`.** `update_document` validated
//!    `title.trim()` and then handed the **untrimmed** string to the repository,
//!    so a 500-character title with a leading space reached `VARCHAR(500)` as
//!    501 characters: `22001 value too long`, surfaced as `internal_error`. The
//!    caller did nothing wrong and there is nothing in the reply to act on.
//! 3. **The same title stored two different ways.** Created, it is trimmed;
//!    renamed, it keeps its padding. The contract says "Non-blank once trimmed"
//!    for both.
//! 4. The refusals *name* characters — "must be 1-500 characters" — while
//!    measuring bytes, so the message actively misleads.
//!
//! The cure is the one `domain::pagination::Pagination` already applies to the
//! page: a value that is **parsed once and travels**. `domain::text::Title` and
//! `domain::text::Comment` can only be built by parsing, the repository takes
//! them rather than `&str`, and there is therefore nowhere left to validate one
//! value and write a different one. Every test below goes through the HTTP
//! boundary, because the boundary is where all four symptoms showed.

mod common;

use actix_web::{test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, payload, versions};
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

/// The contract's numbers, restated here so a change to either side of the
/// wire has to change this file too.
const MAX_TITLE_CHARS: usize = 500;
const MAX_COMMENT_CHARS: usize = 500;
const MAX_METADATA_VALUE_CHARS: usize = 256;

/// Big enough that the payload ceiling never fires: this file is about the
/// field bounds, and a 1 500-byte CJK title must not be refused by the other
/// ceiling instead. See `tests/size_limit_test.rs` for that one.
const BODY_CEILING: usize = 1024 * 1024;

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

/// One character repeated — the point of the file is that *which* character
/// must not change the answer.
fn chars(c: char, n: usize) -> String {
    c.to_string().repeat(n)
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

macro_rules! patch_document {
    ($app:expr, $token:expr, $id:expr, $body:expr) => {{
        let req = test::TestRequest::patch()
            .uri(&format!("/api/v1/documents/{}", $id))
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .set_json($body)
            .to_request();
        test::call_service(&$app, req).await
    }};
}

/// Create an ordinary document and return its id — the fixture the rename
/// tests rewrite.
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

/// What the column actually holds, read back through the API.
macro_rules! stored_title {
    ($app:expr, $token:expr, $id:expr) => {{
        let req = test::TestRequest::get()
            .uri(&format!("/api/v1/documents/{}", $id))
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .to_request();
        let doc: serde_json::Value = test::call_and_read_body_json(&$app, req).await;
        doc["title"].as_str().unwrap().to_string()
    }};
}

/// The promise `maxLength: 500` makes, in the alphabet the product's own
/// examples are written in.
#[actix_web::test]
async fn a_title_of_the_documented_maximum_in_characters_is_stored_whole() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let title = chars('é', MAX_TITLE_CHARS);
    let resp = post_document!(app, token, serde_json::json!({ "title": title }));
    assert_eq!(
        resp.status(),
        201,
        "a title of exactly {MAX_TITLE_CHARS} characters ({} bytes) was refused",
        title.len()
    );

    // Stored whole, not truncated: the round trip is the only proof that the
    // bound admits the value it names.
    let created: serde_json::Value = test::read_body_json(resp).await;
    let id = created["id"].as_str().unwrap();
    let fetched = stored_title!(app, token, id);
    assert_eq!(fetched.chars().count(), MAX_TITLE_CHARS);
    assert_eq!(fetched, title);
}

/// The measurement that names the defect: 500, 250 or 166 characters depending
/// on the alphabet. One bound, one answer.
#[actix_web::test]
async fn the_largest_storable_title_does_not_depend_on_the_alphabet() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    // 1 byte, 2 bytes, 3 bytes and 4 bytes per character.
    for c in ['a', 'é', '合', '𝄞'] {
        let title = chars(c, MAX_TITLE_CHARS);
        let resp = post_document!(app, token, serde_json::json!({ "title": title }));
        assert_eq!(
            resp.status(),
            201,
            "{MAX_TITLE_CHARS} × {c:?} ({} bytes) was refused: the largest storable \
             title still depends on which characters are in it",
            title.len()
        );
    }
}

/// The bound still bounds — otherwise the test above would pass by removing it.
#[actix_web::test]
async fn a_title_over_the_documented_maximum_in_characters_is_refused() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    for c in ['a', 'é', '合'] {
        let title = chars(c, MAX_TITLE_CHARS + 1);
        let resp = post_document!(app, token, serde_json::json!({ "title": title }));
        assert_eq!(
            resp.status(),
            422,
            "{} × {c:?} was accepted above the documented maximum",
            MAX_TITLE_CHARS + 1
        );
    }

    // And on the rename path, which has its own copy of the rule.
    let id = a_document!(app, token);
    let resp = patch_document!(
        app,
        token,
        id,
        serde_json::json!({ "title": chars('é', MAX_TITLE_CHARS + 1) })
    );
    assert_eq!(
        resp.status(),
        422,
        "a rename above the maximum was accepted"
    );
}

/// Measured at `500 {"error":"internal_error"}`: validated trimmed, stored raw,
/// refused by `VARCHAR(500)` two layers below the caller.
#[actix_web::test]
async fn a_title_at_the_maximum_surrounded_by_whitespace_is_not_an_internal_error() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let id = a_document!(app, token);
    let padded = format!(" {} ", chars('a', MAX_TITLE_CHARS));
    let resp = patch_document!(app, token, id, serde_json::json!({ "title": padded }));
    assert_eq!(
        resp.status(),
        200,
        "renaming to a {MAX_TITLE_CHARS}-character title with surrounding whitespace \
         answered {} — the value that was validated is not the value that was stored",
        resp.status()
    );

    let fetched = stored_title!(app, token, id);
    assert_eq!(fetched.chars().count(), MAX_TITLE_CHARS);
    assert_eq!(fetched, chars('a', MAX_TITLE_CHARS));
}

/// "Non-blank once trimmed" is one rule, and creating and renaming are two
/// paths that must not disagree about what it stores.
#[actix_web::test]
async fn a_renamed_title_is_stored_exactly_like_a_created_one() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let padded = "   Contrat de prestation   ";

    let resp = post_document!(app, token, serde_json::json!({ "title": padded }));
    assert_eq!(resp.status(), 201);
    let created: serde_json::Value = test::read_body_json(resp).await;
    let on_creation = stored_title!(app, token, created["id"].as_str().unwrap());

    let id = a_document!(app, token);
    let resp = patch_document!(app, token, id, serde_json::json!({ "title": padded }));
    assert_eq!(resp.status(), 200);
    let on_rename = stored_title!(app, token, id);

    assert_eq!(
        on_rename, on_creation,
        "the same title is stored {on_creation:?} when created and {on_rename:?} when renamed"
    );
    assert_eq!(on_creation, "Contrat de prestation", "it was not trimmed");
}

/// Blank is blank on both paths, whatever the whitespace is made of.
#[actix_web::test]
async fn a_blank_title_is_refused_on_both_paths() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let id = a_document!(app, token);
    for blank in ["", "   ", "\t\n ", "\u{00a0}"] {
        let resp = post_document!(app, token, serde_json::json!({ "title": blank }));
        assert_eq!(resp.status(), 422, "a blank title {blank:?} was created");

        let resp = patch_document!(app, token, id, serde_json::json!({ "title": blank }));
        assert_eq!(resp.status(), 422, "a document was renamed to {blank:?}");
    }
}

/// `comment` carries the same `maxLength: 500` and the same `VARCHAR(500)`.
#[actix_web::test]
async fn a_comment_of_the_documented_maximum_in_characters_is_kept_whole() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let id = a_document!(app, token);
    let comment = chars('é', MAX_COMMENT_CHARS);
    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/documents/{id}/versions"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "comment": comment }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        201,
        "a comment of exactly {MAX_COMMENT_CHARS} characters ({} bytes) was refused",
        comment.len()
    );
    let created: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(created["comment"].as_str().unwrap(), comment);

    // Still bounded.
    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/documents/{id}/versions"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "comment": chars('a', MAX_COMMENT_CHARS + 1) }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        422,
        "a comment above the maximum was accepted"
    );
}

/// The third field the contract bounds in characters: "string values are at
/// most 256 characters".
#[actix_web::test]
async fn a_metadata_value_of_the_documented_maximum_in_characters_is_accepted() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let value = chars('é', MAX_METADATA_VALUE_CHARS);
    let resp = post_document!(
        app,
        token,
        serde_json::json!({ "title": "Métadonnées", "metadata": { "resume": value } })
    );
    assert_eq!(
        resp.status(),
        201,
        "a metadata value of exactly {MAX_METADATA_VALUE_CHARS} characters ({} bytes) was refused",
        value.len()
    );

    let resp = post_document!(
        app,
        token,
        serde_json::json!({
            "title": "Métadonnées",
            "metadata": { "resume": chars('a', MAX_METADATA_VALUE_CHARS + 1) }
        })
    );
    assert_eq!(
        resp.status(),
        422,
        "a metadata value above the maximum was accepted"
    );
}

/// Non-vacuity: the ordinary case this file could have broken while making the
/// extremes work.
#[actix_web::test]
async fn an_ordinary_title_and_comment_still_go_through() {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app!(pool, test_jwt_config());

    let resp = post_document!(
        app,
        token,
        serde_json::json!({ "title": "Contrat de prestation", "content": "<p>Article 1</p>" })
    );
    assert_eq!(resp.status(), 201);
    let created: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(created["title"], "Contrat de prestation");
    let id = created["id"].as_str().unwrap().to_string();

    let resp = patch_document!(
        app,
        token,
        id,
        serde_json::json!({ "title": "Avenant n°3" })
    );
    assert_eq!(resp.status(), 200);
    assert_eq!(stored_title!(app, token, id), "Avenant n°3");

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/documents/{id}/versions"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "comment": "Relecture juridique" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
}
