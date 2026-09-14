//! The query string and the path: the inputs nothing at this boundary parsed.
//!
//! Every other input a caller can type is turned into a value at the boundary
//! before anything looks at it — `Title`, `Comment`, `Metadata`, `Pagination`.
//! The query string and the path parameters were not: `serde` typed them, and
//! whatever `serde` refused was answered by the framework, in the framework's
//! shape, from a layer this service never wrote. Measured on the running binary
//! on 2026-09-14, with a valid token, against the published contract:
//!
//! ```text
//! GET ?page=abc              400 text/plain  Query deserialize error: invalid digit found in string
//! GET ?per_page=5.5          400 text/plain  Query deserialize error: invalid digit found in string
//! GET ?page=9999999999999999 400 text/plain  Query deserialize error: number too large to fit in target type
//! GET ?page=                 400 text/plain  Query deserialize error: cannot parse integer from empty string
//! GET /documents/not-a-uuid  404 text/plain  UUID parsing failed: invalid character: found `n` at 1
//! GET /versions/abc          404 text/plain  can not parse "abc" to a i32
//! GET /api/v1/nope           404 (no body, no content-type)
//! ```
//!
//! `docs/openapi.yaml` publishes exactly one error shape —
//! `{"error": …, "message": …}` with `error` drawn from a **closed**
//! enumeration — and `AC-031` says that enumeration is exact, "ni plus, ni
//! moins". Seven responses above are outside it. A client that parses the error
//! body, which is what a generated client does, gets a parse failure instead of
//! the message.
//!
//! This is the same seam `api::payload` closed for the JSON body on 2026-09-14
//! (`413`/`400` arrived as `text/plain` until `JsonConfig` was given an error
//! handler) — two extractors over, and still uncovered because
//! `tests/error_surface.rs` reads `src/error.rs` and the contract, never the
//! wire.
//!
//! And one behaviour rather than a shape, on the same query string. Four
//! parameters, one gesture — *the field was left empty* — and four different
//! answers:
//!
//! ```text
//! ?page=      400 text/plain    ?status=   400 application/json
//! ?per_page=  400 text/plain    ?search=   200 with an EMPTY page
//! ```
//!
//! The last is the worst of the four and it is silent: a tenant owning three
//! documents is told it owns none, which is precisely the reading
//! `tests/list_contract_test.rs` refused for an unknown `?status=` ("a typo
//! must not read as *you own no documents*"). The rule applied here is the one
//! this repository already applies to headers in `api::middleware::correlate`:
//! **a value that is blank once trimmed is an absent value**, never an error
//! and never a filter.
//!
//! And no wider than that: a value that is *not* blank travels exactly as it
//! was sent. `?status=published%20` stays the `400` the list contract chose for
//! it on purpose — trimming it into a valid status would have overturned a
//! neighbouring decision while claiming to fix this one.

mod common;

use actix_web::{http::StatusCode, test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, payload, versions};
use ods_doceditor::config::DEFAULT_MAX_DOCUMENT_BYTES;
use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::collections::BTreeSet;
use std::sync::Arc;
use uuid::Uuid;

const OPENAPI: &str = include_str!("../docs/openapi.yaml");

/// The values of `Error.error` in the published contract — the only codes this
/// service may put on the wire (AC-031).
///
/// Read from the contract rather than restated here, so this file cannot drift
/// from `docs/openapi.yaml` and from `tests/error_surface.rs`, which holds the
/// other half of the same promise.
fn documented_error_codes() -> BTreeSet<String> {
    let schema = OPENAPI
        .split_once("\n    Error:\n")
        .expect("docs/openapi.yaml must declare an `Error` schema")
        .1;
    let list = schema
        .split_once("enum:\n")
        .expect("the Error schema must list its error codes as an enum")
        .1;

    list.lines()
        .map(str::trim)
        .take_while(|line| line.starts_with("- "))
        .map(|line| line.trim_start_matches("- ").to_string())
        .collect()
}

fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/api/v1/documents",
        web::get().to(documents::list_documents),
    )
    .route(
        "/api/v1/documents/{id}",
        web::get().to(documents::get_document),
    )
    .route(
        "/api/v1/documents/{doc_id}/versions/{version}",
        web::get().to(versions::get_version),
    );
}

struct Fixture {
    pool: sqlx::PgPool,
    svc: DocumentService,
    token: String,
}

/// The app, wired through `payload::limits` — the one function `main.rs` calls.
///
/// Load-bearing, and the reason lot 9 exists: a bench that builds its own
/// boundary is a bench from which the component under test can be absent
/// entirely. Every refusal asserted below comes from that call.
macro_rules! app {
    ($fx:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new($fx.pool.clone()))
                .app_data(web::Data::new($fx.svc.clone()))
                .app_data(web::Data::new(test_jwt_config()))
                .configure(payload::limits(DEFAULT_MAX_DOCUMENT_BYTES))
                .configure(routes),
        )
        .await
    };
}

/// Status, content type and parsed body of a refusal — read in that order, so a
/// `text/plain` answer fails on the shape rather than on a JSON parse panic.
macro_rules! response {
    ($app:expr, $token:expr, $uri:expr) => {{
        let req = test::TestRequest::get()
            .uri($uri)
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .to_request();
        let res = test::call_service(&$app, req).await;
        let status = res.status();
        let content_type = res
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();
        let bytes = test::read_body(res).await;
        let text = String::from_utf8_lossy(&bytes).to_string();
        (status, content_type, text)
    }};
}

/// Assert a refusal speaks this service's published error shape.
fn assert_service_error_shape(uri: &str, content_type: &str, body: &str) -> serde_json::Value {
    assert!(
        content_type.starts_with("application/json"),
        "{uri} answered `{content_type}` with {body:?}; docs/openapi.yaml publishes exactly one \
         error shape and it is application/json"
    );
    let parsed: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("{uri} answered a body that is not JSON ({e}): {body:?}"));

    let code = parsed["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{uri} answered without an `error` field: {body:?}"));
    assert!(
        documented_error_codes().contains(code),
        "{uri} answered error code {code:?}, which docs/openapi.yaml does not list among the \
         values of Error.error — AC-031 says that enumeration is exact"
    );
    assert!(
        parsed["message"].is_string(),
        "{uri} answered without a `message`: {body:?}"
    );
    parsed
}

/// Three documents, one of which is the only one mentioning `résolutoire`.
async fn tenant_with_three_documents() -> Fixture {
    let pool = setup_test_pool().await;
    let svc = DocumentService::new(pool.clone(), Arc::new(InMemoryProducer::new()));

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    for (n, body) in [
        "Le present contrat comporte une clause resolutoire.",
        "Le present contrat est conclu pour un an.",
        "Le present contrat regit la prestation.",
    ]
    .iter()
    .enumerate()
    {
        svc.create_document(
            tenant_id,
            &format!("Contrat de prestation {n}"),
            user_id,
            &Metadata::empty(),
            Some(body),
            None,
        )
        .await
        .expect("seeding a document must succeed");
    }

    Fixture { pool, svc, token }
}

/// A parameter left empty is a parameter that was not supplied.
///
/// `?page=&per_page=&status=&search=` is what a form submits when the user
/// typed in none of its fields, and it is one gesture, so it must have one
/// answer. It had four: two `400 text/plain`, one `400 application/json` and
/// one `200` with an empty page.
#[actix_web::test]
async fn a_blank_query_parameter_is_an_absent_one() {
    let fx = tenant_with_three_documents().await;
    let token = fx.token.clone();
    let app = app!(fx);

    // Non-vacuity: the same request with nothing in the query string at all.
    let (status, _, body) = response!(app, token, "/api/v1/documents");
    assert_eq!(status, StatusCode::OK);
    let bare: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(bare["total"], 3, "the fixture must have three documents");
    assert_eq!(bare["page"], 1);
    assert_eq!(bare["per_page"], 20);

    for query in [
        "page=",
        "per_page=",
        "status=",
        "search=",
        "page=&per_page=&status=&search=",
        // Blank once trimmed is blank: a space is what a URL-encoded empty
        // field becomes as soon as anything pads it.
        "search=%20%20",
        "status=%20",
    ] {
        let uri = format!("/api/v1/documents?{query}");
        let (status, content_type, body) = response!(app, token, &uri);
        assert_eq!(
            status,
            StatusCode::OK,
            "?{query} names no value, so it must answer like the bare request; got {status} \
             {content_type} {body:?}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["total"], 3,
            "?{query} must not filter anything out: the tenant owns three documents"
        );
        assert_eq!(parsed["page"], 1, "?{query} must serve the default page");
        assert_eq!(
            parsed["per_page"], 20,
            "?{query} must serve the default page size"
        );
    }
}

/// An empty search is not a search that matches nothing.
///
/// This is the one refusal of the four that is not an error at all: `200` with
/// an empty page, which reads to a client as *you own no documents*. The same
/// reading was refused for `?status=` on 2026-09-14 — a plausible answer is
/// worse than a rejection, because nothing surfaces.
#[actix_web::test]
async fn an_empty_search_is_not_a_search_that_matches_nothing() {
    let fx = tenant_with_three_documents().await;
    let token = fx.token.clone();
    let app = app!(fx);

    macro_rules! total {
        ($query:expr) => {{
            let uri = format!("/api/v1/documents?{}", $query);
            let (status, content_type, body) = response!(app, token, &uri);
            assert_eq!(
                status,
                StatusCode::OK,
                "?{} {content_type} {body:?}",
                $query
            );
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            parsed["total"].as_i64().unwrap()
        }};
    }

    // Non-vacuity, both ways: search really does filter, and a word nobody
    // wrote really does answer an empty page.
    assert_eq!(total!("search=resolutoire"), 1, "one document says it");
    assert_eq!(
        total!("search=zzzzunlikely"),
        0,
        "and no document says this"
    );

    assert_eq!(
        total!("search="),
        3,
        "an empty search box is not a query that matches nothing"
    );
    assert_eq!(total!("search=%20"), 3, "and neither is a blank one");
}

/// A number this service cannot read is refused in this service's own shape.
///
/// The status was already right — `400` is what the contract documents for this
/// route — but the body was the framework's, in `text/plain`, and named the
/// serde error rather than the parameter.
#[actix_web::test]
async fn a_malformed_number_is_refused_in_the_services_own_error_shape() {
    let fx = tenant_with_three_documents().await;
    let token = fx.token.clone();
    let app = app!(fx);

    for (query, parameter) in [
        ("page=abc", "page"),
        ("per_page=5.5", "per_page"),
        ("page=99999999999999999999", "page"),
        ("per_page=-", "per_page"),
    ] {
        let uri = format!("/api/v1/documents?{query}");
        let (status, content_type, body) = response!(app, token, &uri);

        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "?{query} is not a number this service can read"
        );
        let parsed = assert_service_error_shape(&uri, &content_type, &body);
        assert_eq!(parsed["error"], "bad_request");
        assert!(
            parsed["message"].as_str().unwrap().contains(parameter),
            "?{query} must tell the caller WHICH parameter it could not read: {body:?}"
        );
    }

    // Non-vacuity: a number it can read is not refused.
    let (status, _, _) = response!(app, token, "/api/v1/documents?page=2&per_page=2");
    assert_eq!(status, StatusCode::OK);
}

/// An identifier the router cannot parse is refused in this service's own shape.
///
/// `404` is the right status and stays: a path that cannot name a resource
/// names no resource, and AC-028 requires an unknown id to be indistinguishable
/// from another tenant's. What changes is that the answer is JSON, like every
/// other `404` this service produces.
#[actix_web::test]
async fn a_malformed_path_parameter_is_refused_in_the_services_own_error_shape() {
    let fx = tenant_with_three_documents().await;
    let token = fx.token.clone();
    let app = app!(fx);

    let live = Uuid::new_v4();
    for uri in [
        "/api/v1/documents/not-a-uuid".to_string(),
        "/api/v1/documents/12345".to_string(),
        format!("/api/v1/documents/{live}/versions/abc"),
        // An `i32` this does not fit in — a number a client reaches by counting.
        format!("/api/v1/documents/{live}/versions/99999999999"),
    ] {
        let (status, content_type, body) = response!(app, token, &uri);
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{uri} names no resource, so it is a 404"
        );
        let parsed = assert_service_error_shape(&uri, &content_type, &body);
        assert_eq!(parsed["error"], "not_found");
    }

    // Non-vacuity: a well-formed id that names nothing answers the same way,
    // which is the whole point — the two must be indistinguishable.
    let uri = format!("/api/v1/documents/{live}");
    let (status, content_type, body) = response!(app, token, &uri);
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_service_error_shape(&uri, &content_type, &body);
}

/// A request that matches no route answers in this service's own shape too.
///
/// The last body on the wire that was not JSON: actix's own default service
/// answers `404` with no body and no content type at all.
#[actix_web::test]
async fn an_unmatched_route_is_refused_in_the_services_own_error_shape() {
    let fx = tenant_with_three_documents().await;
    let token = fx.token.clone();
    let app = app!(fx);

    for uri in ["/api/v1/nope", "/", "/api/v1/documents/"] {
        let (status, content_type, body) = response!(app, token, uri);
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        let parsed = assert_service_error_shape(uri, &content_type, &body);
        assert_eq!(parsed["error"], "not_found");
    }
}

/// The guard is not vacuous: the contract really was read.
///
/// Declared `async` for one reason only: `use actix_web::test` shadows the
/// `#[test]` attribute in this file.
#[actix_web::test]
async fn the_documented_error_codes_were_actually_read() {
    let codes = documented_error_codes();
    assert!(
        codes.len() >= 3,
        "read only {} error code(s) from docs/openapi.yaml: {codes:?}",
        codes.len()
    );
    assert!(codes.contains("not_found"), "{codes:?}");
    assert!(codes.contains("bad_request"), "{codes:?}");
    assert!(
        !codes.contains("Query deserialize error"),
        "the parser, not the contract, is what changed: {codes:?}"
    );
}
