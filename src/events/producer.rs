use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// CloudEvents v1.0 envelope for editor domain events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudEvent {
    pub specversion: String,
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub time: DateTime<Utc>,
    pub datacontenttype: String,
    pub tenantid: String,
    pub data: serde_json::Value,
}

impl CloudEvent {
    pub fn new(event_type: &str, tenant_id: Uuid, data: serde_json::Value) -> Self {
        Self {
            specversion: "1.0".to_string(),
            id: format!("evt-{}", Uuid::new_v4()),
            source: "/editor".to_string(),
            event_type: event_type.to_string(),
            time: Utc::now(),
            datacontenttype: "application/json".to_string(),
            tenantid: tenant_id.to_string(),
            data,
        }
    }

    pub fn document_created(
        tenant_id: Uuid,
        document_id: Uuid,
        title: &str,
        created_by: Uuid,
    ) -> Self {
        Self::new(
            "com.ods.editor.document.created",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "title": title,
                "created_by": created_by,
            }),
        )
    }

    pub fn document_updated(
        tenant_id: Uuid,
        document_id: Uuid,
        updated_by: Uuid,
        changes: Vec<&str>,
        version: i32,
    ) -> Self {
        Self::new(
            "com.ods.editor.document.updated",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "updated_by": updated_by,
                "changes": changes,
                "version": version,
            }),
        )
    }

    pub fn document_deleted(tenant_id: Uuid, document_id: Uuid, deleted_by: Uuid) -> Self {
        Self::new(
            "com.ods.editor.document.deleted",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "deleted_by": deleted_by,
            }),
        )
    }

    pub fn document_published(
        tenant_id: Uuid,
        document_id: Uuid,
        published_by: Uuid,
        version: i32,
    ) -> Self {
        Self::new(
            "com.ods.editor.document.published",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "published_by": published_by,
                "version": version,
            }),
        )
    }

    pub fn version_created(
        tenant_id: Uuid,
        document_id: Uuid,
        version: i32,
        created_by: Uuid,
        is_auto: bool,
    ) -> Self {
        Self::new(
            "com.ods.editor.version.created",
            tenant_id,
            serde_json::json!({
                "document_id": document_id,
                "tenant_id": tenant_id,
                "version": version,
                "created_by": created_by,
                "is_auto": is_auto,
            }),
        )
    }
}

/// Trait for event publishing (allows mocking in tests).
pub trait EventProducer: Send + Sync {
    fn publish(&self, event: CloudEvent) -> Result<(), String>;
}

/// In-memory producer for testing.
#[derive(Debug, Default, Clone)]
pub struct InMemoryProducer {
    pub events: Arc<Mutex<Vec<CloudEvent>>>,
}

impl InMemoryProducer {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn get_events(&self) -> Vec<CloudEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl EventProducer for InMemoryProducer {
    fn publish(&self, event: CloudEvent) -> Result<(), String> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

/// No-op producer when Redpanda is not configured.
pub struct NoopProducer;

impl EventProducer for NoopProducer {
    fn publish(&self, _event: CloudEvent) -> Result<(), String> {
        tracing::debug!("Event dropped (no Redpanda configured)");
        Ok(())
    }
}
