mod common;

use actix_web::{test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{
    generate_expired_token, generate_test_token, test_jwt_config,
};
use ods_doceditor::api::{documents, health, versions};
use ods_doceditor::domain::document::DocumentUpdate;
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

/// AC-020: Health endpoint returns 200 with status ok.
#[actix_web::test]
async fn test_health_endpoint() {
    let app = test::init_service(App::new().route("/health", web::get().to(health::health))).await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "doceditor");
}

/// AC-001: Create a document with valid title, get status draft and version 1.
#[actix_web::test]
async fn test_create_document() {
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
            .route(
                "/api/v1/documents",
                web::post().to(documents::create_document),
            ),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "My First Document"
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["title"], "My First Document");
    assert_eq!(body["status"], "draft");
    assert_eq!(body["current_version"], 1);
    assert_eq!(body["tenant_id"], tenant_id.to_string());
    assert_eq!(body["created_by"], user_id.to_string());

    // Verify events were emitted: the document, and the version 1 it starts with.
    let events = producer.get_events();
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "com.ods.editor.document.created",
            "com.ods.editor.version.created"
        ]
    );
}

/// AC-001: Reject document creation with empty title (BR-001).
#[actix_web::test]
async fn test_create_document_empty_title_rejected() {
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
            .route(
                "/api/v1/documents",
                web::post().to(documents::create_document),
            ),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "   "
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 422);
}

/// AC-002: List only returns documents belonging to the authenticated tenant.
#[actix_web::test]
async fn test_list_documents_tenant_isolation() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    // Create doc for tenant A
    svc.create_document(
        tenant_a,
        "Doc A",
        user_id,
        serde_json::json!({}),
        None,
        None,
    )
    .await
    .unwrap();

    // Create doc for tenant B
    svc.create_document(
        tenant_b,
        "Doc B",
        user_id,
        serde_json::json!({}),
        None,
        None,
    )
    .await
    .unwrap();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents",
                web::get().to(documents::list_documents),
            ),
    )
    .await;

    // List as tenant A: should only see Doc A
    let token_a = generate_test_token(user_id, tenant_a);
    let req = test::TestRequest::get()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = test::read_body_json(resp).await;
    let docs = body["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["title"], "Doc A");
}

/// AC-003: PATCH status from draft to published works.
#[actix_web::test]
async fn test_update_document_status_draft_to_published() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let doc = svc
        .create_document(
            tenant_id,
            "To Publish",
            user_id,
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .unwrap();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents/{id}",
                web::patch().to(documents::update_document),
            ),
    )
    .await;

    let req = test::TestRequest::patch()
        .uri(&format!("/api/v1/documents/{}", doc.id))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "status": "published" }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "published");

    // Verify published event was emitted
    let events = producer.get_events();
    let published_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "com.ods.editor.document.published")
        .collect();
    assert_eq!(published_events.len(), 1);
}

/// AC-004: PATCH status from published to draft is rejected.
#[actix_web::test]
async fn test_update_document_invalid_status_transition() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    // Create and publish
    let doc = svc
        .create_document(
            tenant_id,
            "Published Doc",
            user_id,
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .unwrap();
    svc.update_document(
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            status: Some("published"),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents/{id}",
                web::patch().to(documents::update_document),
            ),
    )
    .await;

    // Try to go back to draft (should fail)
    let req = test::TestRequest::patch()
        .uri(&format!("/api/v1/documents/{}", doc.id))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "status": "draft" }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

/// AC-021: DELETE soft-deletes and emits event.
#[actix_web::test]
async fn test_delete_document_soft_delete() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let doc = svc
        .create_document(
            tenant_id,
            "To Delete",
            user_id,
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .unwrap();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc.clone()))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents/{id}",
                web::delete().to(documents::delete_document),
            )
            .route(
                "/api/v1/documents",
                web::get().to(documents::list_documents),
            ),
    )
    .await;

    // Delete
    let req = test::TestRequest::delete()
        .uri(&format!("/api/v1/documents/{}", doc.id))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 204);

    // Verify not in list anymore
    let req = test::TestRequest::get()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();

    let resp = test::call_service(&app, req).await;
    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["total"], 0);

    // Verify delete event was emitted
    let events = producer.get_events();
    let delete_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "com.ods.editor.document.deleted")
        .collect();
    assert_eq!(delete_events.len(), 1);
}

/// AC-005 (versioning): Create a version snapshot.
#[actix_web::test]
async fn test_create_and_list_versions() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let doc = svc
        .create_document(
            tenant_id,
            "Versioned Doc",
            user_id,
            serde_json::json!({}),
            None,
            None,
        )
        .await
        .unwrap();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents/{id}/versions",
                web::post().to(versions::create_version),
            )
            .route(
                "/api/v1/documents/{id}/versions",
                web::get().to(versions::list_versions),
            ),
    )
    .await;

    // Create a version
    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/documents/{}/versions", doc.id))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "comment": "First save" }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = test::read_body_json(resp).await;
    // Version 1 is written at creation, so the first explicit snapshot is 2.
    assert_eq!(body["version"], 2);
    assert_eq!(body["comment"], "First save");

    // List versions
    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{}/versions", doc.id))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = test::read_body_json(resp).await;
    let versions = body["versions"].as_array().unwrap();
    assert_eq!(
        versions.len(),
        2,
        "version 1 (creation) + version 2 (snapshot)"
    );
    assert_eq!(versions[0]["version"], 2, "most recent first");
    assert_eq!(versions[1]["version"], 1);
}

/// Test: unauthenticated request returns 401 (no Authorization header).
#[actix_web::test]
async fn test_unauthenticated_request() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents",
                web::get().to(documents::list_documents),
            ),
    )
    .await;

    // No auth headers
    let req = test::TestRequest::get()
        .uri("/api/v1/documents")
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

/// Test: expired token returns 401.
#[actix_web::test]
async fn test_expired_token_rejected() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_expired_token(user_id, tenant_id);

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents",
                web::get().to(documents::list_documents),
            ),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

/// Test: invalid token (garbage) returns 401.
#[actix_web::test]
async fn test_invalid_token_rejected() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc))
            .app_data(web::Data::new(jwt_cfg))
            .route(
                "/api/v1/documents",
                web::get().to(documents::list_documents),
            ),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", "Bearer totally.invalid.token"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

/// BR-029: Invalid metadata key rejected.
#[actix_web::test]
async fn test_invalid_metadata_key_rejected() {
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
            .route(
                "/api/v1/documents",
                web::post().to(documents::create_document),
            ),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/api/v1/documents")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "title": "Test",
            "metadata": { "InvalidKey": "value" }
        }))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 422);
}
