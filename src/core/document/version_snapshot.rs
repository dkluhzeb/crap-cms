//! Document snapshot representation for versioning.

use crate::core::document::VersionSnapshotBuilder;
use serde::{Deserialize, Serialize};

/// A version snapshot of a document, stored in the `_versions_{slug}` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionSnapshot {
    /// The unique identifier for this version snapshot.
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
    /// The timestamp when this version was created.
    pub created_at: Option<String>,
    /// The timestamp when this version was last updated.
    pub updated_at: Option<String>,
}

impl VersionSnapshot {
    /// Returns a new `VersionSnapshotBuilder` for constructing a snapshot.
    pub fn builder(id: impl Into<String>, parent: impl Into<String>) -> VersionSnapshotBuilder {
        VersionSnapshotBuilder::new(id, parent)
    }
}
