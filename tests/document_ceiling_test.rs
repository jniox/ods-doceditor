//! The ceiling a human chose, and the four places that have to agree on it.
//!
//! `MAX_DOCUMENT_SIZE_MB` is not a taste: it is one of three settings that only
//! make sense together — the body ceiling, the instance memory, and the number
//! of requests admitted at once. Measured on 2026-09-14 on the running binary,
//! in a cgroup set to the deployment's own allocation
//! (`ops/cloudrun/doceditor.json`: `memory 512Mi`), N concurrent full-size
//! saves of distinct documents:
//!
//! ```text
//! ceiling 10 MB, N=10  -> 200 x10, peak 451 MiB
//! ceiling 10 MB, N=12  -> Result=oom-kill, MainPID=0, 11 of 12 get no response
//! ceiling  4 MB, N=80  -> Result=oom-kill, MainPID=0
//! ceiling  3 MB, N=80  -> 200 x80, peak 475 MiB   (93% of the cap: no margin)
//! ceiling  2 MB, N=80  -> 200 x80, peak 340 MiB
//! ```
//!
//! **80 is not an arbitrary N**: it is Cloud Run's own default number of
//! concurrent requests per instance, and `ops/cloudrun/doceditor.json` sets no
//! `--concurrency`, so it is the number in force. At 10 MB the deployment could
//! be killed — the *instance*, so every other tenant's in-flight request with
//! it — by eleven ordinary saves. At 2 MB the platform's own default fits, with
//! a third of the memory to spare.
//!
//! HR-20260914-001, option A, decided 2026-09-14 by it@orbusdigital.com:
//! "Baisser MAX_DOCUMENT_SIZE_MB (ex. 2 Mo) … Exécutant : dev, dans le dépôt
//! doceditor." The dispatcher could not route it (`verbe inconnu`), so it is
//! enacted here.
//!
//! What this file guards is not the number itself but the **agreement**: the
//! code's default, the environment template a deployer copies, and the contract
//! a client reads all name the same ceiling, or the build goes red. A published
//! maximum that only the source knows is the defect this repository has now hit
//! four times in four batches (a limit applied to the encoding rather than the
//! body, a bound counted in bytes and published in characters, an index budget
//! deciding what could be stored, an allowance whose test restated its own
//! prose).

mod common;

use actix_web::{test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, payload};
use ods_doceditor::config::{DEFAULT_MAX_DOCUMENT_BYTES, DEFAULT_MAX_DOCUMENT_SIZE_MB};
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

const ENV_TEMPLATE: &str = include_str!("../.env.example");
const OPENAPI: &str = include_str!("../docs/openapi.yaml");

/// The three statements of the same ceiling, checked against each other.
///
/// In its own module because the file-level `use actix_web::test` shadows the
/// standard `#[test]` attribute — a plain synchronous test then fails to
/// compile with "the async keyword is missing from the function declaration",
/// which says nothing about the real cause.
mod agreement {
    use super::{DEFAULT_MAX_DOCUMENT_BYTES, DEFAULT_MAX_DOCUMENT_SIZE_MB, ENV_TEMPLATE, OPENAPI};

    /// The value HR-20260914-001 settled, restated where a reader will look for it.
    #[test]
    fn the_default_ceiling_is_the_one_the_sizing_decision_settled() {
        assert_eq!(
            DEFAULT_MAX_DOCUMENT_SIZE_MB, 2,
            "HR-20260914-001 option A lowered the body ceiling to 2 MB. Measured at 512 MiB and \
             Cloud Run's default concurrency of 80: 2 MB -> 340 MiB, 3 MB -> 475 MiB, 4 MB -> \
             oom-kill. Changing this number is a sizing decision, not a refactor: measure it \
             again and open a human review."
        );
    }

    /// The template a deployer copies names the number the code applies.
    ///
    /// `.env.example` is not documentation here — `CLAUDE.md` requires it to list
    /// exactly what `src/config.rs` reads, and it is what a human edits before a
    /// deployment. A template still advertising the old ceiling is how an operator
    /// re-raises a limit a measurement lowered, believing they are keeping it.
    #[test]
    fn the_environment_template_names_the_default_the_code_applies() {
        let expected = format!("MAX_DOCUMENT_SIZE_MB={DEFAULT_MAX_DOCUMENT_SIZE_MB}");
        assert!(
            ENV_TEMPLATE.contains(&expected),
            ".env.example must set {expected}; it names a different ceiling from the one \
             src/config.rs defaults to"
        );
    }

    /// The published contract states the ceiling as a **number**, not only as the
    /// name of an environment variable the caller cannot read.
    ///
    /// Until this test, `docs/openapi.yaml` said "bounded by `MAX_DOCUMENT_SIZE_MB`"
    /// in three places and never once said how large that was — so the one thing a
    /// client integrating against the contract needed to know was the one thing the
    /// contract withheld.
    #[test]
    fn the_published_contract_states_the_ceiling_as_a_number() {
        let bytes = DEFAULT_MAX_DOCUMENT_BYTES.to_string();
        assert!(
            OPENAPI.contains(&bytes),
            "docs/openapi.yaml must state the default ceiling in bytes ({bytes}); naming only the \
             environment variable tells a client nothing it can act on"
        );
    }

    /// The bound is in **bytes of UTF-8**, and the contract must not express it
    /// with `maxLength`, which counts characters.
    ///
    /// This is the trap of batch 10, one field over: `title` really is bounded
    /// in characters and rightly carries `maxLength: 500`; `content` is bounded
    /// by `str::len()`, so the same annotation would promise a Chinese document
    /// three times the size this service will store, and a generated client
    /// would validate against a number the server does not use.
    #[test]
    fn the_contract_does_not_express_a_byte_bound_as_a_character_bound() {
        for schema in ["CreateDocumentRequest", "UpdateDocumentRequest"] {
            let content = property_block(&schema_block(schema), "content");
            // The YAML key, not the word: the description says in prose why
            // this field has no `maxLength`, and that sentence must not be
            // what makes the test pass.
            let declares_max_length = content
                .lines()
                .any(|line| line.trim_start().starts_with("maxLength:"));
            assert!(
                !declares_max_length,
                "{schema}.content is bounded in bytes; maxLength counts characters and would \
                 publish a ceiling three times too large for a non-ASCII body"
            );
            assert!(
                content.contains(&DEFAULT_MAX_DOCUMENT_BYTES.to_string()),
                "{schema}.content must say how large a body may be, in the unit the service \
                 counts: bytes"
            );
        }
    }

    /// The `components/schemas` entry of that name, up to the next one.
    fn schema_block(name: &str) -> String {
        let from = OPENAPI
            .split_once(&format!("\n    {name}:\n"))
            .unwrap_or_else(|| panic!("docs/openapi.yaml must describe {name}"))
            .1;
        match from.find("\n    ") {
            // A line indented by exactly four spaces starts the next schema.
            Some(_) => from
                .split_inclusive('\n')
                .take_while(|line| {
                    !(line.starts_with("    ") && !line.starts_with("     ") && line.trim() != "")
                })
                .collect(),
            None => from.to_string(),
        }
    }

    /// One property of a schema block, up to the next property at its level.
    fn property_block(schema: &str, property: &str) -> String {
        let from = schema
            .split_once(&format!("\n        {property}:\n"))
            .unwrap_or_else(|| panic!("the schema must describe a {property} property"))
            .1;
        from.split_inclusive('\n')
            .take_while(|line| {
                !(line.starts_with("        ")
                    && !line.starts_with("         ")
                    && line.trim() != "")
            })
            .collect()
    }
}

macro_rules! app_at_the_default_ceiling {
    ($pool:expr, $jwt:expr) => {{
        // No `with_max_content_bytes`: this is the wiring an operator who sets
        // nothing gets, which is exactly what `ops/cloudrun/doceditor.json`
        // does — it sets no MAX_DOCUMENT_SIZE_MB at all.
        let svc = DocumentService::new($pool.clone(), Arc::new(InMemoryProducer::new()));
        test::init_service(
            App::new()
                .app_data(web::Data::new($pool.clone()))
                .app_data(web::Data::new(svc))
                .app_data(web::Data::new($jwt))
                .configure(payload::limits(DEFAULT_MAX_DOCUMENT_BYTES))
                .route(
                    "/api/v1/documents",
                    web::post().to(documents::create_document),
                ),
        )
        .await
    }};
}

async fn create_with_body(bytes: usize) -> actix_web::http::StatusCode {
    let pool = setup_test_pool().await;
    let token = generate_test_token(Uuid::new_v4(), Uuid::new_v4());
    let app = app_at_the_default_ceiling!(pool, test_jwt_config());

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "ceiling probe",
            "content": "x".repeat(bytes),
        }))
        .to_request();
    test::call_service(&app, req).await.status()
}

/// Non-vacuity: the default really is enforced, and it really does admit a body
/// of exactly its own size — the promise the variable's name makes.
#[actix_web::test]
async fn a_body_of_exactly_the_default_ceiling_is_stored() {
    assert_eq!(
        create_with_body(DEFAULT_MAX_DOCUMENT_BYTES).await,
        201,
        "a body of exactly {DEFAULT_MAX_DOCUMENT_BYTES} bytes is the documented maximum and \
         must be storable"
    );
}

/// And one byte more is the service's own refusal, naming the field — not the
/// framework's `413`, which says only that the request was too long to read.
#[actix_web::test]
async fn one_byte_over_the_default_ceiling_is_the_services_own_refusal() {
    assert_eq!(
        create_with_body(DEFAULT_MAX_DOCUMENT_BYTES + 1).await,
        422,
        "a body over the ceiling must be refused by the service with a 422 naming the limit"
    );
}
