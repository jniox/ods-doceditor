use uuid::Uuid;

/// A document template: the starting body a new document can be created from.
///
/// Only the fields the creation path needs are selected; the table carries more
/// (description, category, timestamps) but nothing reads them yet, and widening
/// this struct without a reader would recreate the dead-field problem it fixes.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Template {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub name: String,
    pub content_html: String,
    pub is_system: bool,
}
