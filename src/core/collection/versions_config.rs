//! Document versioning and draft configuration for a collection.

use serde::{Deserialize, Serialize};

/// Configuration for document versioning and drafts on a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionsConfig {
    /// Enable draft/publish workflow with `_status` field.
    #[serde(default)]
    pub drafts: bool,
    /// Maximum versions to keep per document (0 = unlimited).
    #[serde(default)]
    pub max_versions: u32,
}

impl VersionsConfig {
    /// Create a new versioning configuration.
    pub fn new(drafts: bool, max_versions: u32) -> Self {
        Self {
            drafts,
            max_versions,
        }
    }
}
