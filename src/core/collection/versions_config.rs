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

    /// The prune cap a definition's optional versions config sets.
    ///
    /// No config at all — a collection without versioning — and an explicit
    /// `0` both mean unlimited, so every lifecycle step that writes a version
    /// derives the cap the same way instead of re-deciding what a missing
    /// config means.
    #[must_use]
    pub fn cap(versions: Option<&Self>) -> u32 {
        versions.map_or(0, |v| v.max_versions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_sets_fields() {
        let c = VersionsConfig::new(true, 5);
        assert!(c.drafts);
        assert_eq!(c.max_versions, 5);
    }

    /// A collection without versioning and one with an explicit `0` both mean
    /// "keep everything", so no prune caller has to special-case either.
    #[test]
    fn cap_treats_no_config_and_zero_alike() {
        assert_eq!(VersionsConfig::cap(None), 0);
        assert_eq!(VersionsConfig::cap(Some(&VersionsConfig::new(true, 0))), 0);
        assert_eq!(VersionsConfig::cap(Some(&VersionsConfig::new(true, 7))), 7);
    }

    #[test]
    fn serde_defaults_to_no_drafts_and_unlimited_versions() {
        let c: VersionsConfig = serde_json::from_value(json!({})).unwrap();
        assert!(!c.drafts);
        assert_eq!(c.max_versions, 0, "0 means unlimited");
    }
}
