//! AC-019 — DocEditor stores the document's actual content, not just its metadata.
//! AC-018 — `template_id` seeds that content instead of being silently discarded.
//!
//! Before this batch the service accepted no content field at all, `yjs_state`
//! was read but never written, and every version snapshot was an empty byte
//! array. These tests pin the authoring path end to end.

mod common;

use actix_web::{test, web, App};
use common::{insert_template, setup_test_pool};
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, versions};
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

fn app_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
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
        web::get().to(versions::list_versions),
    )
    .route(
        "/api/v1/documents/{doc_id}/versions/{version}",
        web::get().to(versions::get_version),
    );
}

/// AC-019: content sent on creation is stored, returned, word-counted, and
/// captured as version 1 — the version a product can later restore.
#[actix_web::test]
async fn test_create_document_persists_content_and_seeds_version_one() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .configure(app_routes),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "Contrat de prestation",
            "content": "<h1>Article 1</h1><p>Les parties conviennent.</p>"
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    let created: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(
        created["content"], "<h1>Article 1</h1><p>Les parties conviennent.</p>",
        "the content body must be stored and echoed back"
    );
    assert_eq!(created["current_version"], 1);
    // 4 and not 6: `word_count` splits on whitespace and does not strip markup.
    assert_eq!(created["word_count"], 4, "word_count must reflect the body");

    let doc_id = created["id"].as_str().unwrap().to_string();

    // The content survives a round trip.
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{doc_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let fetched: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(fetched["content"], created["content"]);

    // Version 1 exists and is not an empty snapshot.
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{doc_id}/versions/1"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let v1: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(v1["version"], 1);
    assert_eq!(v1["content"], created["content"]);
    assert!(
        v1["snapshot_size_bytes"].as_i64().unwrap() > 0,
        "a version snapshot must carry bytes, not an empty array"
    );
}

/// AC-019: a content mutation produces a new immutable version, and the prior
/// version keeps the prior content (GTM: "create + 2 edits" = 3 records).
#[actix_web::test]
async fn test_update_content_creates_new_immutable_version() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .configure(app_routes),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "title": "Draft", "content": "first body" }))
        .to_request();
    let created: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    let doc_id = created["id"].as_str().unwrap().to_string();

    for (n, body) in [(2, "second body"), (3, "third body")] {
        let req = test::TestRequest::patch()
            .uri(&format!("/api/v1/documents/{doc_id}"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "content": body }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let updated: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(updated["content"], body);
        assert_eq!(
            updated["current_version"], n,
            "each content mutation must advance the version"
        );
    }

    // Three records: creation + two edits.
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{doc_id}/versions"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let listed: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(listed["versions"].as_array().unwrap().len(), 3);

    // Immutability: version 1 still holds the original body.
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{doc_id}/versions/1"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let v1: serde_json::Value = test::call_and_read_body_json(&app, req).await;
    assert_eq!(v1["content"], "first body");

    // The update is announced on the event stream with `content` among the changes.
    let changed: Vec<_> = producer
        .get_events()
        .into_iter()
        .filter(|e| e.event_type == "com.ods.editor.document.updated")
        .collect();
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[0].data["changes"], serde_json::json!(["content"]));
}

/// AC-019: `MAX_DOCUMENT_SIZE_MB` is enforced on the body, not only on the
/// HTTP payload — a product must get a typed 422, not a truncated document.
#[actix_web::test]
async fn test_content_over_the_configured_limit_is_rejected() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone()).with_max_content_bytes(64);
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .configure(app_routes),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "title": "Too big", "content": "x".repeat(65) }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 422);
}

/// AC-018: a document created from a template starts from the template body.
#[actix_web::test]
async fn test_create_document_from_template_seeds_the_content() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);
    let template_id = insert_template(
        &pool,
        Some(tenant_id),
        "Contrat type",
        "<h1>Modèle</h1>",
        user_id,
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .configure(app_routes),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "title": "From template", "template_id": template_id }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    let created: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(created["content"], "<h1>Modèle</h1>");
}

/// AC-018: an unknown template is refused rather than silently ignored, and a
/// template belonging to another tenant is unknown.
#[actix_web::test]
async fn test_unknown_or_foreign_template_is_refused() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let other_tenant = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);
    let foreign_template = insert_template(
        &pool,
        Some(other_tenant),
        "Pas le mien",
        "<p>secret</p>",
        user_id,
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .configure(app_routes),
    )
    .await;

    for candidate in [Uuid::new_v4(), foreign_template] {
        let req = test::TestRequest::post()
            .uri("/api/v1/documents")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "title": "Nope", "template_id": candidate }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            404,
            "template {candidate} must not be usable"
        );
    }
}

/// AC-018: `content` and `template_id` together is a client error, not a
/// silent precedence rule nobody can guess.
#[actix_web::test]
async fn test_content_and_template_together_is_rejected() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);
    let template_id =
        insert_template(&pool, Some(tenant_id), "T", "<p>from template</p>", user_id).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .configure(app_routes),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "Ambiguous",
            "content": "<p>mine</p>",
            "template_id": template_id
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

/// A tenant cannot read another tenant's document content (defence in depth).
#[actix_web::test]
async fn test_content_is_not_readable_across_tenants() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let doc = svc
        .create_document(
            tenant_a,
            "Confidentiel",
            user_id,
            serde_json::json!({}),
            Some("clauses secrètes"),
            None,
        )
        .await
        .unwrap();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .configure(app_routes),
    )
    .await;

    let token_b = generate_test_token(user_id, tenant_b);
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{}", doc.id))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);

    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{}/versions/1", doc.id))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}
