//! What a soft-delete must mean on EVERY read path, not just the obvious one.
//!
//! `DELETE /api/v1/documents/{id}` is advertised as removing the document. The
//! document read and the version *list* both honoured that; the version *read*
//! did not — and it is the one route that returns the body. So a deleted
//! document's full content stayed retrievable at
//! `GET /api/v1/documents/{id}/versions/{n}`, with `n` starting at 1 and
//! increasing by one, i.e. guessable without any knowledge of the history.
//!
//! The shape to recognise: three sibling read paths, one of them written
//! without the `deleted_at IS NULL` predicate its neighbours carry. A per-route
//! test suite finds it only if it asks the same question of every route, which
//! is why these assertions are a loop over the routes rather than one case for
//! the route somebody happened to think about.

mod common;

use actix_web::{test, web, App};
use common::setup_test_pool;
use ods_doceditor::api::extractors::test_helpers::{generate_test_token, test_jwt_config};
use ods_doceditor::api::{documents, versions};
use ods_doceditor::domain::document::DocumentUpdate;
use ods_doceditor::domain::metadata::Metadata;
use ods_doceditor::events::producer::InMemoryProducer;
use ods_doceditor::service::document_service::DocumentService;
use std::sync::Arc;
use uuid::Uuid;

fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
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
        "/api/v1/documents/{id}/versions",
        web::post().to(versions::create_version),
    )
    .route(
        "/api/v1/documents/{doc_id}/versions/{version}",
        web::get().to(versions::get_version),
    );
}

/// Build a document with a real history (v1 from creation, v2 from an edit) so
/// that "read a prior version" has something to read on both sides of the
/// delete.
async fn seeded_document(svc: &DocumentService, tenant_id: Uuid, user_id: Uuid) -> Uuid {
    let doc = svc
        .create_document(
            tenant_id,
            "Quarterly report",
            user_id,
            &Metadata::empty(),
            Some("<p>first draft, confidential</p>"),
            None,
        )
        .await
        .expect("creation must succeed");

    svc.update_document(
        tenant_id,
        doc.id,
        user_id,
        DocumentUpdate {
            title: None,
            status: None,
            metadata: None,
            content: Some("<p>second draft, still confidential</p>"),
        },
    )
    .await
    .expect("the edit must succeed");

    doc.id
}

/// The regression itself: after the delete, no read path serves the document,
/// and in particular not the one that returns the body.
#[actix_web::test]
async fn test_soft_deleted_document_is_gone_from_every_read_path() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let doc_id = seeded_document(&svc, tenant_id, user_id).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc.clone()))
            .app_data(web::Data::new(jwt_cfg))
            .configure(routes),
    )
    .await;

    // Non-vacuity: every one of these reads works *before* the delete. Without
    // this, a route that 404s for an unrelated reason (a typo in the URI, a
    // missing route registration) would make the assertions below pass while
    // proving nothing at all.
    for uri in [
        format!("/api/v1/documents/{doc_id}"),
        format!("/api/v1/documents/{doc_id}/versions"),
        format!("/api/v1/documents/{doc_id}/versions/1"),
        format!("/api/v1/documents/{doc_id}/versions/2"),
    ] {
        let req = test::TestRequest::get()
            .uri(&uri)
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            200,
            "{uri} must be readable before the delete, otherwise this test proves nothing"
        );
    }

    svc.delete_document(tenant_id, doc_id, user_id)
        .await
        .expect("the delete must succeed");

    for uri in [
        format!("/api/v1/documents/{doc_id}"),
        format!("/api/v1/documents/{doc_id}/versions"),
        format!("/api/v1/documents/{doc_id}/versions/1"),
        format!("/api/v1/documents/{doc_id}/versions/2"),
    ] {
        let req = test::TestRequest::get()
            .uri(&uri)
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        assert_eq!(
            test::call_service(&app, req).await.status(),
            404,
            "{uri} still serves a deleted document"
        );
    }
}

/// The body is the thing that must stop being served — asserted on the payload
/// and not only on the status code, so that a 404 carrying the content in its
/// error body could not pass.
#[actix_web::test]
async fn test_deleted_document_body_is_not_served_by_the_version_read() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let doc_id = seeded_document(&svc, tenant_id, user_id).await;
    svc.delete_document(tenant_id, doc_id, user_id)
        .await
        .expect("the delete must succeed");

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc.clone()))
            .app_data(web::Data::new(jwt_cfg))
            .configure(routes),
    )
    .await;

    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{doc_id}/versions/1"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);

    let body = test::read_body(resp).await;
    let body = String::from_utf8_lossy(&body);
    assert!(
        !body.contains("confidential"),
        "the deleted document's body leaked in the response: {body}"
    );
}

/// The write path of the same resource. `create_version` already carried the
/// predicate; pinning it means the next person who touches these two
/// neighbouring functions cannot fix one and regress the other.
#[actix_web::test]
async fn test_cannot_snapshot_or_edit_a_soft_deleted_document() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let doc_id = seeded_document(&svc, tenant_id, user_id).await;
    svc.delete_document(tenant_id, doc_id, user_id)
        .await
        .expect("the delete must succeed");

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc.clone()))
            .app_data(web::Data::new(jwt_cfg))
            .configure(routes),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/api/v1/documents/{doc_id}/versions"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "comment": "after the grave" }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        404,
        "an explicit snapshot of a deleted document must not be accepted"
    );

    let req = test::TestRequest::patch()
        .uri(&format!("/api/v1/documents/{doc_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "content": "<p>resurrected</p>" }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        404,
        "editing a deleted document must not be accepted"
    );
}

/// Deleting one document must not take its tenant-mates' history with it: the
/// predicate added to `get_version` filters on the document, not on the tenant.
#[actix_web::test]
async fn test_deleting_one_document_leaves_the_others_readable() {
    let pool = setup_test_pool().await;
    let producer = Arc::new(InMemoryProducer::new());
    let svc = DocumentService::new(pool.clone(), producer.clone());
    let jwt_cfg = test_jwt_config();

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let token = generate_test_token(user_id, tenant_id);

    let doomed = seeded_document(&svc, tenant_id, user_id).await;
    let survivor = seeded_document(&svc, tenant_id, user_id).await;

    svc.delete_document(tenant_id, doomed, user_id)
        .await
        .expect("the delete must succeed");

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(svc.clone()))
            .app_data(web::Data::new(jwt_cfg))
            .configure(routes),
    )
    .await;

    let req = test::TestRequest::get()
        .uri(&format!("/api/v1/documents/{survivor}/versions/1"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        200,
        "an untouched document lost its history when a sibling was deleted"
    );
}
