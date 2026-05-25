//! Document versioning and draft configuration for a collection.

use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAnnotation;

/// Configuration for document versioning and drafts on a collection.
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.VersionsConfig")]
pub struct VersionsConfig {
    /// Enable draft/publish workflow (default: false). Adds `_status` column.
    #[serde(default)]
    #[lua(optional)]
    pub drafts: bool,
    /// Maximum version snapshots to keep per document (default: unlimited).
    #[serde(default)]
    #[lua(optional)]
    pub max_versions: u32,
}

impl VersionsConfig {
    /// Create a new versioning configuration.
    #[must_use]
    pub fn new(drafts: bool, max_versions: u32) -> Self {
        Self {
            drafts,
            max_versions,
        }
    }
}
