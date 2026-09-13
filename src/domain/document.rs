use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Valid document status values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentStatus {
    Draft,
    Published,
    Archived,
}

impl DocumentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "published" => Some(Self::Published),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    /// Validate status transition per BR-002:
    /// draft -> published, published -> archived, draft -> archived.
    pub fn can_transition_to(&self, target: &Self) -> bool {
        matches!(
            (self, target),
            (Self::Draft, Self::Published)
                | (Self::Published, Self::Archived)
                | (Self::Draft, Self::Archived)
        )
    }
}

/// Domain model for a document.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Document {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: String,
    pub status: String,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub current_version: i32,
    pub word_count: i32,
    pub metadata: serde_json::Value,
}

/// Domain model for a document version.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub tenant_id: Uuid,
    pub version: i32,
    pub yjs_snapshot: Vec<u8>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub comment: Option<String>,
    pub snapshot_size_bytes: i32,
    pub is_auto: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_status_transitions() {
        assert!(DocumentStatus::Draft.can_transition_to(&DocumentStatus::Published));
        assert!(DocumentStatus::Draft.can_transition_to(&DocumentStatus::Archived));
        assert!(DocumentStatus::Published.can_transition_to(&DocumentStatus::Archived));
    }

    #[test]
    fn test_invalid_status_transitions() {
        // published -> draft is NOT allowed (AC-004)
        assert!(!DocumentStatus::Published.can_transition_to(&DocumentStatus::Draft));
        // archived -> anything is NOT allowed
        assert!(!DocumentStatus::Archived.can_transition_to(&DocumentStatus::Draft));
        assert!(!DocumentStatus::Archived.can_transition_to(&DocumentStatus::Published));
        // same status
        assert!(!DocumentStatus::Draft.can_transition_to(&DocumentStatus::Draft));
    }

    #[test]
    fn test_status_from_str() {
        assert_eq!(DocumentStatus::parse("draft"), Some(DocumentStatus::Draft));
        assert_eq!(
            DocumentStatus::parse("published"),
            Some(DocumentStatus::Published)
        );
        assert_eq!(
            DocumentStatus::parse("archived"),
            Some(DocumentStatus::Archived)
        );
        assert_eq!(DocumentStatus::parse("unknown"), None);
    }
}
