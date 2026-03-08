use crate::core::document::VersionSnapshotBuilder;
use serde::{Deserialize, Serialize};

/// A version snapshot of a document, stored in the `_versions_{slug}` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionSnapshot {
    pub id: String,
    /// The parent document ID in the main collection table.
    pub parent: String,
    /// Sequential version number (1, 2, 3...).
    pub version: i64,
    /// Status at the time of this snapshot (`"draft"` or `"published"`).
    pub status: String,
    /// Whether this is the latest version for the parent document.
    pub latest: bool,
    /// Full document data as a JSON object.
    pub snapshot: serde_json::Value,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl VersionSnapshot {
    pub fn builder(id: impl Into<String>, parent: impl Into<String>) -> VersionSnapshotBuilder {
        VersionSnapshotBuilder::new(id, parent)
    }
}
