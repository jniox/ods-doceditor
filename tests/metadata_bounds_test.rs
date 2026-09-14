//! What bounds `metadata` — asked of the shapes a caller can actually send.
//!
//! `docs/openapi.yaml` bounds this field in prose: *at most 20 keys, keys match
//! `^[a-z][a-z0-9_]{0,63}$`, string values are at most 256 characters*. Nothing
//! in that sentence mentions depth, and `src/api/payload.rs` relies on it by
//! name: `ENVELOPE_ALLOWANCE_BYTES` is documented as covering "the largest
//! envelope **this service's own validation admits** — a 500-byte title, twenty
//! metadata keys of 64 bytes holding 256-byte values … under 7 KiB".
//!
//! Measured on the running binary (release, `MAX_DOCUMENT_SIZE_MB=10`, HS256,
//! port 8199) before this file existed:
//!
//! ```text
//! POST metadata {"resume": 257 × 'a'}            -> 422 "must be at most 256 characters"
//! POST metadata {"resume": {"inner": 257 × 'a'}} -> 201
//! POST metadata {"tags": [257 × 'a']}            -> 201
//! POST metadata {"resume": {"inner": 5 000 000 × 'a'}} -> 201
//! ```
//!
//! The rule is stated about string values and applied to string values **at
//! depth 1**: wrapping the same string in an object or an array is enough to
//! store five megabytes in a field whose documented maximum is 256 characters.
//! And because no rule bounds the object as a whole, the only remaining ceiling
//! is the payload one — `2 × body + 64 KiB`, i.e. 20 MiB — which is derived from
//! the assumption this file's first paragraph quotes. The premise is false by a
//! factor of three hundred, and the unit test that guards it computes the
//! envelope from the prose instead of from the constants the code enforces.
//!
//! What that costs is not a refused request. `DocumentSummary` carries
//! `metadata` — the list projection deliberately drops `content` and keeps this
//! — so a page multiplies it by up to a hundred. Measured on the same binary,
//! 30 documents of 10 MB of metadata each (30 ordinary `201`s, empty bodies):
//!
//! ```text
//! GET /api/v1/documents?per_page=30 -> 200, 300 010 939 bytes, peak RSS 945 MiB
//! ```
//!
//! against the 512 MiB `ops/cloudrun/doceditor.json` allocates. That is an OOM
//! kill of the instance, so every other tenant's in-flight request dies with
//! it — the same ending as the version history in ADR-007, reached through the
//! one column that read kept.
//!
//! Two rules, then, and they are not redundant: the character bound holds
//! whatever *container* a string sits in, and the size bound holds whatever
//! *shape* the value takes. Either alone leaves a hole — a million 256-character
//! strings in an array satisfies the first; one 5 000-character string
//! satisfies the second.

mod common;

use actix_web::{test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, payload};
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

/// The contract's numbers, restated here so a change to either side of the wire
/// has to change this file too.
const MAX_METADATA_VALUE_CHARS: usize = 256;
const MAX_METADATA_BYTES: usize = 32 * 1024;

/// Large enough that the body ceiling never fires: this file is about the
/// metadata rules, and the oversized envelopes below carry no body at all.
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
                    "/api/v1/documents",
                    web::get().to(documents::list_documents),
                )
                .route(
                    "/api/v1/documents/{id}",
                    web::patch().to(documents::update_document),
                ),
        )
        .await
    }};
}

fn chars(c: char, n: usize) -> String {
    c.to_string().repeat(n)
}

/// The four containers a string can reach the `jsonb` column through.
///
/// Named rather than inlined because the point of this file is that the rule
/// must not depend on which of them the caller picks.
fn metadata_holding(value: serde_json::Value) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "a value of the metadata object",
            serde_json::json!({ "resume": value }),
        ),
        (
            "a value nested in an object",
            serde_json::json!({ "resume": { "inner": value } }),
        ),
        (
            "an element of an array",
            serde_json::json!({ "tags": [value] }),
        ),
        (
            "an element of an array of objects",
            serde_json::json!({ "tags": [{ "label": value }] }),
        ),
    ]
}

/// A creation carrying `metadata`, and the status it gets back.
///
/// A macro and not a function: `init_service` returns an opaque type this crate
/// cannot name without depending on `actix-http` directly.
macro_rules! post_metadata {
    ($app:expr, $token:expr, $title:expr, $metadata:expr) => {{
        let req = test::TestRequest::post()
            .uri("/api/v1/documents")
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .set_json(serde_json::json!({ "title": $title, "metadata": $metadata }))
            .to_request();
        test::call_service(&$app, req).await.status().as_u16()
    }};
}

/// The bound the contract states, asked of every container.
#[actix_web::test]
async fn the_documented_character_bound_holds_wherever_the_string_sits() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app!(pool, test_jwt_config());

    let too_long = serde_json::Value::String(chars('a', MAX_METADATA_VALUE_CHARS + 1));
    for (where_it_sits, metadata) in metadata_holding(too_long) {
        let status = post_metadata!(app, token, "over", metadata);
        assert_eq!(
            status,
            422,
            "a {} character string was accepted as {where_it_sits} — the documented maximum is {}",
            MAX_METADATA_VALUE_CHARS + 1,
            MAX_METADATA_VALUE_CHARS
        );
    }

    // Non-vacuity: the rule must refuse the string, not the container. A
    // service that answered 422 to every shape would pass the loop above.
    let at_the_bound = serde_json::Value::String(chars('a', MAX_METADATA_VALUE_CHARS));
    for (where_it_sits, metadata) in metadata_holding(at_the_bound) {
        let status = post_metadata!(app, token, "at the bound", metadata);
        assert_eq!(
            status, 201,
            "a string of exactly {MAX_METADATA_VALUE_CHARS} characters was refused as \
             {where_it_sits}"
        );
    }
}

/// Characters, not bytes — the same question `tests/text_bounds_test.rs` asks of
/// the title, asked here of a value one level down, where the byte/character
/// confusion would be invisible.
#[actix_web::test]
async fn the_character_bound_is_counted_in_characters_at_depth_too() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app!(pool, test_jwt_config());

    for alphabet in ['é', '漢'] {
        let value = serde_json::Value::String(chars(alphabet, MAX_METADATA_VALUE_CHARS));
        let status = post_metadata!(
            app,
            token,
            "accented",
            serde_json::json!({ "resume": { "inner": value } })
        );
        assert_eq!(
            status, 201,
            "{MAX_METADATA_VALUE_CHARS} × '{alphabet}' was refused against a bound the contract \
             counts in characters"
        );
    }
}

/// Metadata built entirely of values each rule above accepts, and too large as
/// a whole. This is the shape the character bound cannot see.
fn metadata_of_about(bytes: usize) -> serde_json::Value {
    let chunk = chars('a', MAX_METADATA_VALUE_CHARS);
    let per_element = MAX_METADATA_VALUE_CHARS + 3; // the string, its quotes, a comma
    let elements = bytes / per_element;
    serde_json::json!({ "tags": vec![chunk; elements] })
}

#[actix_web::test]
async fn metadata_is_bounded_as_a_whole_and_not_only_value_by_value() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app!(pool, test_jwt_config());

    let over = metadata_of_about(MAX_METADATA_BYTES * 2);
    let serialised = serde_json::to_vec(&over).unwrap().len();
    assert!(
        serialised > MAX_METADATA_BYTES,
        "the fixture is not actually over the bound ({serialised} bytes)"
    );
    assert_eq!(
        post_metadata!(app, token, "over as a whole", over),
        422,
        "{serialised} bytes of metadata were accepted against a bound of {MAX_METADATA_BYTES}; \
         every string in it is within the character bound, which is why that rule cannot see it"
    );

    // Non-vacuity: the bound must admit what it promises.
    let under = metadata_of_about(MAX_METADATA_BYTES / 2);
    let serialised = serde_json::to_vec(&under).unwrap().len();
    assert!(serialised < MAX_METADATA_BYTES);
    assert_eq!(
        post_metadata!(app, token, "under as a whole", under),
        201,
        "{serialised} bytes of metadata were refused against a bound of {MAX_METADATA_BYTES}"
    );
}

/// The rename path is a write too, and it is where the last bound in this
/// repository turned out to be missing (see `domain::text::Title`).
#[actix_web::test]
async fn the_update_path_is_bounded_by_the_same_rules() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app!(pool, test_jwt_config());

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "title": "created small" }))
        .to_request();
    let created: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    let id = created["id"].as_str().expect("no id in the creation reply");

    for (label, metadata) in [
        // 300 000 characters and not the 5 000 000 measured on the running
        // binary: this bench installs a 1 MiB body ceiling, so a 5 MB envelope
        // is refused by the payload limit (413) before the rule under test is
        // reached. A test must exercise the boundary it names.
        (
            "a 300 000 character string one level down",
            serde_json::json!({ "resume": { "inner": chars('a', 300_000) } }),
        ),
        (
            "an array over the size bound",
            metadata_of_about(MAX_METADATA_BYTES * 2),
        ),
    ] {
        let req = test::TestRequest::patch()
            .uri(&format!("/api/v1/documents/{id}"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "metadata": metadata }))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status().as_u16(),
            422,
            "PATCH accepted {label}: a bound that holds on creation and not on rename is not a \
             bound on what is stored"
        );
    }
}

/// What the bound is *for*: the cost of a page.
///
/// `DocumentSummary` drops `content` on purpose and keeps `metadata`. Before
/// this batch, a page of thirty documents weighed 300 MB and peaked at 945 MiB
/// of RSS against a 512 MiB instance. The assertion below is the arithmetic
/// that makes that impossible: whatever a caller stores, a page cannot exceed
/// the page size times the rule.
///
/// Said plainly, because the same shape misled a reviewer on ADR-007: **this
/// test passes before the fix as well**. It cannot store what it cannot send,
/// so it can only ever measure admissible metadata. It is kept because it is
/// the *consequence* the write bounds exist for, and because it would go red
/// the day the list grows a column the bounds do not cover — but the tests that
/// discriminate are the three above it.
#[actix_web::test]
async fn a_page_cannot_weigh_more_than_the_page_size_times_the_rule() {
    let pool = setup_test_pool().await;
    let (tenant_id, user_id) = (Uuid::new_v4(), Uuid::new_v4());
    let token = generate_test_token(user_id, tenant_id);
    let app = app!(pool, test_jwt_config());

    // As much metadata as the rule admits, on every document of the page.
    let biggest_admissible = metadata_of_about(MAX_METADATA_BYTES - 2048);
    let documents = 3;
    for i in 0..documents {
        assert_eq!(
            post_metadata!(
                app,
                token,
                format!("page cost {i}"),
                biggest_admissible.clone()
            ),
            201
        );
    }

    let req = test::TestRequest::get()
        .uri("/api/v1/documents?per_page=100")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let body = test::call_and_read_body(&app, req).await;

    // The per-document envelope: the metadata bound plus the summary's own
    // fields (ids, timestamps, title, counters) — generous, because the claim
    // is about the order of magnitude, not about a byte.
    let per_document = MAX_METADATA_BYTES + 2048;
    assert!(
        body.len() <= documents * per_document,
        "a page of {documents} documents weighs {} bytes, more than the {} the rule allows — \
         metadata is unbounded again",
        body.len(),
        documents * per_document
    );

    // Non-vacuity: the page really did carry the metadata it was given, so the
    // bound above is not satisfied by an empty answer.
    assert!(
        body.len() > documents * (MAX_METADATA_BYTES / 2),
        "the page weighs only {} bytes: it did not return the metadata, so bounding it proves \
         nothing",
        body.len()
    );
}
