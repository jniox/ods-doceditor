//! What `GET /api/v1/documents` promises, measured against what it does.
//!
//! Four findings, one seam. The endpoint normalises the caller's input in
//! `DocumentService` (`page.max(1)`, `per_page.clamp(1, 100)`) and reports it
//! back in `api::documents::list_documents` — two modules that never compare
//! notes. Everything below is a consequence of that split, and none of it was
//! visible to the existing clamp test, which calls the *service* and therefore
//! never sees the response the client reads.
//!
//! 1. The response echoes the numbers the caller SENT, not the ones the server
//!    USED. `?per_page=1000` answers `"per_page": 1000` with at most 100
//!    documents, which contradicts `docs/openapi.yaml` (`maximum: 100`) and
//!    breaks any client computing `ceil(total / per_page)`. `?page=0` answers
//!    `"page": 0` while serving page 1, so a client walking 0, 1, 2 reads the
//!    first page twice.
//! 2. A large `page` overflows `(page - 1) * per_page` in
//!    `document_repo::list_documents`: a panic on a debug build, and on a
//!    release build (the Dockerfile's) a wrap to `OFFSET -200`, which
//!    PostgreSQL refuses — a 500 produced by a number a client is free to send.
//! 3. `DocumentService::validate_metadata` enforces BR-029 (at most 20 keys,
//!    `^[a-z][a-z0-9_]{0,63}$`, values under 256 chars) only when the metadata
//!    IS a JSON object, and silently accepts everything else. A guard that
//!    returns `Ok` for the inputs it was not shaped for is not a guard.
//! 4. `?status=` outside the documented enum answers `200` with an empty page,
//!    which reads to a client as "you have no documents" rather than "that is
//!    not a status". PATCH already answers 400 for the same word.
//!
//! Each test carries its non-vacuity case: an assertion that only fires on the
//! defect is indistinguishable from one that never fires at all.

mod common;

use actix_web::{http::StatusCode, test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::documents;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/api/v1/documents",
        web::get().to(documents::list_documents),
    )
    .route(
        "/api/v1/documents",
        web::post().to(documents::create_document),
    );
}

/// A tenant with `count` documents, plus everything needed to serve it.
struct Fixture {
    pool: sqlx::PgPool,
    svc: DocumentService,
    token: String,
}

/// Build the app for a fixture. A macro and not a function: `init_service`
/// returns an opaque type this crate cannot name without depending on
/// `actix-http` directly.
macro_rules! app {
    ($fx:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new($fx.pool.clone()))
                .app_data(web::Data::new($fx.svc.clone()))
                .app_data(web::Data::new(test_jwt_config()))
                .configure(routes),
        )
        .await
    };
}

async fn tenant_with_documents(count: usize) -> Fixture {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer);

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    for n in 0..count {
        svc.create_document(
            tenant_id,
            &format!("Doc {n}"),
            user_id,
            &Metadata::empty(),
            None,
            None,
        )
        .await
        .expect("seeding a document must succeed");
    }

    Fixture { pool, svc, token }
}

/// The page metadata in the response describes the page that was served.
///
/// Not the page that was requested: the two differ exactly when the server
/// normalised something, which is the only case where the client needs to be
/// told.
#[actix_web::test]
async fn the_response_reports_the_page_it_actually_served() {
    let fx = tenant_with_documents(3).await;
    let token = &fx.token;
    let app = app!(fx);

    macro_rules! list {
        ($query:expr) => {{
            let req = test::TestRequest::get()
                .uri(&format!("/api/v1/documents?{}", $query))
                .insert_header(("Authorization", format!("Bearer {token}")))
                .to_request();
            test::call_and_read_body_json::<_, _, serde_json::Value>(&app, req).await
        }};
    }

    // Non-vacuity: an in-range request is echoed unchanged, so the assertions
    // below cannot pass by the endpoint simply ignoring its input.
    let honoured = list!("page=2&per_page=2");
    assert_eq!(honoured["page"], 2, "an in-range page is served as asked");
    assert_eq!(honoured["per_page"], 2);
    assert_eq!(honoured["documents"].as_array().unwrap().len(), 1);

    // `per_page` is clamped to 100 (the contract says so, and the service does
    // it) — so 100 is what the response must report.
    let clamped = list!("per_page=1000");
    assert_eq!(
        clamped["per_page"], 100,
        "the response must report the per_page that was applied, not the one that was asked for \
         — docs/openapi.yaml declares maximum: 100, and a client computing ceil(total/per_page) \
         from 1000 pages wrongly"
    );
    assert!(
        clamped["documents"].as_array().unwrap().len() <= 100,
        "the page itself is clamped, which is precisely why echoing 1000 is a lie"
    );

    // `page` is floored at 1 — so 1 is what the response must report.
    let floored = list!("page=0&per_page=2");
    assert_eq!(
        floored["page"], 1,
        "page 0 is served as page 1, so it must be reported as page 1 — a client walking \
         0, 1, 2 would otherwise read the first page twice"
    );
    assert_eq!(
        floored["documents"].as_array().unwrap().len(),
        2,
        "and the page really is the first one"
    );

    let negative = list!("page=-5&per_page=2");
    assert_eq!(negative["page"], 1);
}

/// A page number past the end is an empty page, never a server error.
///
/// `(page - 1) * per_page` overflows `i64` well before the caller runs out of
/// digits: a panic under `debug`, and under `release` a wrap to a negative
/// `OFFSET` that PostgreSQL rejects with `2201X`, surfacing as a 500 on a
/// request the contract says is merely out of range.
#[actix_web::test]
async fn a_page_number_past_the_end_is_an_empty_page_not_a_server_error() {
    let fx = tenant_with_documents(3).await;
    let token = &fx.token;
    let app = app!(fx);

    for page in [i64::MAX, i64::MAX / 2, 1_000_000_000_000] {
        let req = test::TestRequest::get()
            .uri(&format!("/api/v1/documents?page={page}&per_page=100"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let res = test::call_service(&app, req).await;

        assert_eq!(
            res.status(),
            StatusCode::OK,
            "page={page} must answer an empty page, not a server error"
        );
        let body: serde_json::Value = test::read_body_json(res).await;
        assert!(
            body["documents"].as_array().unwrap().is_empty(),
            "page={page} is past the end of a 3-document collection"
        );
        assert_eq!(
            body["total"], 3,
            "total still counts the collection, whatever page was asked for"
        );
    }
}

/// BR-029 applies to the metadata field, not only to the metadata field when it
/// happens to be an object.
///
/// `serde_json::Value::as_object()` answers `None` for a string, a number or an
/// array, and the guard then returns `Ok` — so every rule it enforces (20 keys,
/// key shape, 256-char values) is bypassed by sending anything that is not a
/// map. The column is `jsonb`; it accepts all of them.
#[actix_web::test]
async fn metadata_that_is_not_an_object_is_refused() {
    let fx = tenant_with_documents(0).await;
    let token = &fx.token;
    let app = app!(fx);

    macro_rules! create {
        ($metadata:expr) => {{
            let req = test::TestRequest::post()
                .uri("/api/v1/documents")
                .insert_header(("Authorization", format!("Bearer {token}")))
                .set_json(serde_json::json!({
                    "title": "Metadata probe",
                    "metadata": $metadata,
                }))
                .to_request();
            test::call_service(&app, req).await.status()
        }};
    }

    // Non-vacuity: the shape BR-029 was written for is still accepted.
    assert_eq!(
        create!(serde_json::json!({"project": "ods", "stage": "p4"})),
        StatusCode::CREATED,
        "a well-formed metadata object must still be accepted"
    );
    assert_eq!(
        create!(serde_json::json!({})),
        StatusCode::CREATED,
        "an empty object is the documented default"
    );

    for rejected in [
        serde_json::json!("a string sneaks past every key rule"),
        serde_json::json!(["so", "does", "an", "array"]),
        serde_json::json!(42),
        serde_json::json!(true),
    ] {
        assert_eq!(
            create!(rejected.clone()),
            StatusCode::UNPROCESSABLE_ENTITY,
            "metadata {rejected} is not an object, so none of BR-029's rules can apply to it — \
             accepting it stores an unvalidated value the contract says is an object"
        );
    }
}

/// A status filter outside the documented enum is refused, not silently empty.
///
/// `docs/openapi.yaml` types this parameter as `DocumentStatus`. Answering 200
/// with an empty page tells a client with a typo that it owns no documents,
/// which is the one answer that is both wrong and plausible. PATCH already
/// answers 400 for the same word.
#[actix_web::test]
async fn an_unknown_status_filter_is_refused_rather_than_silently_empty() {
    let fx = tenant_with_documents(3).await;
    let token = &fx.token;
    let app = app!(fx);

    macro_rules! list {
        ($status:expr) => {{
            let req = test::TestRequest::get()
                .uri(&format!("/api/v1/documents?status={}", $status))
                .insert_header(("Authorization", format!("Bearer {token}")))
                .to_request();
            test::call_service(&app, req).await
        }};
    }

    // Non-vacuity: every documented value is still served.
    for status in ["draft", "published", "archived"] {
        assert_eq!(
            list!(status).status(),
            StatusCode::OK,
            "{status} is a documented status and must be served"
        );
    }
    let drafts: serde_json::Value = test::read_body_json(list!("draft")).await;
    assert_eq!(drafts["total"], 3, "the three seeded documents are drafts");

    // `published%20` is a trailing space, percent-encoded: a value that
    // *looks* like a status and is not one is the realistic typo.
    for unknown in ["deleted", "Draft", "DRAFT", "published%20"] {
        assert_eq!(
            list!(unknown).status(),
            StatusCode::BAD_REQUEST,
            "'{unknown}' is not a DocumentStatus; answering 200 with an empty page reads as \
             'you have no documents'"
        );
    }
}
