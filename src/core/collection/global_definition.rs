//! Global definitions — single-document collections.

use crate::core::{
    FieldDefinition, Slug,
    collection::{Access, Hooks, Labels, LiveMode, LiveSetting, McpConfig, VersionsConfig},
};
use serde::{Deserialize, Serialize};

/// Global definitions are simpler — single-document collections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalDefinition {
    /// Unique identifier for the global.
    pub slug: Slug,
    /// Human-readable labels for the global.
    #[serde(default)]
    pub labels: Labels,
    /// List of fields that make up the global's schema.
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    /// Lua hook functions triggered during various lifecycle events.
    #[serde(default)]
    pub hooks: Hooks,
    /// Access control rules for reading and updating the global.
    #[serde(default)]
    pub access: Access,
    /// Model Context Protocol (MCP) configuration for AI integration.
    #[serde(default)]
    pub mcp: McpConfig,
    /// Real-time update settings for this global.
    #[serde(default)]
    pub live: Option<LiveSetting>,
    /// Controls what data events carry (metadata-only or full with after_read hooks).
    #[serde(default)]
    pub live_mode: LiveMode,
    /// Versioning and draft configuration.
    #[serde(default)]
    pub versions: Option<VersionsConfig>,
}

impl Default for GlobalDefinition {
    fn default() -> Self {
        Self {
            slug: Slug::new(""),
            labels: Labels::default(),
            fields: Vec::new(),
            hooks: Hooks::default(),
            access: Access::default(),
            mcp: McpConfig::default(),
            live: None,
            live_mode: LiveMode::default(),
            versions: None,
        }
    }
}

impl GlobalDefinition {
    /// Create a new `GlobalDefinition` with the given slug and default settings.
    pub fn new(slug: impl Into<Slug>) -> Self {
        Self {
            slug: slug.into(),
            ..Default::default()
        }
    }

    /// Create a builder for `GlobalDefinition`.
    pub fn builder(slug: impl Into<Slug>) -> super::GlobalDefinitionBuilder {
        super::GlobalDefinitionBuilder::new(slug)
    }

    /// Get the display label (singular form, falls back to slug). Uses default resolution.
    pub fn display_name(&self) -> &str {
        self.labels
            .singular
            .as_ref()
            .map(|ls| ls.resolve_default())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the display label resolved for a specific locale.
    #[allow(dead_code)]
    pub fn display_name_for(&self, locale: &str, default_locale: &str) -> &str {
        self.labels
            .singular
            .as_ref()
            .map(|ls| ls.resolve(locale, default_locale))
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Check if this global has versioning enabled.
    pub fn has_versions(&self) -> bool {
        self.versions.is_some()
    }

    /// Check if this global has drafts enabled (versioning with drafts flag).
    pub fn has_drafts(&self) -> bool {
        self.versions.as_ref().is_some_and(|v| v.drafts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::LocalizedString;
    use std::collections::HashMap;

    fn make_global(slug: &str, singular: Option<&str>) -> GlobalDefinition {
        let mut def = GlobalDefinition::new(slug);
        def.labels = Labels {
            singular: singular.map(|s| LocalizedString::Plain(s.to_string())),
            plural: None,
        };
        def
    }

    #[test]
    fn global_display_name_uses_singular_label() {
        let g = make_global("site_settings", Some("Site Settings"));
        assert_eq!(g.display_name(), "Site Settings");
    }

    #[test]
    fn global_display_name_falls_back_to_slug() {
        let g = make_global("site_settings", None);
        assert_eq!(g.display_name(), "site_settings");
    }

    #[test]
    fn global_display_name_empty_falls_back_to_slug() {
        let g = make_global("site_settings", Some(""));
        assert_eq!(g.display_name(), "site_settings");
    }

    #[test]
    fn global_display_name_for_locale() {
        let mut labels = HashMap::new();
        labels.insert("en".to_string(), "Site Settings".to_string());
        labels.insert("de".to_string(), "Seiteneinstellungen".to_string());
        let mut g = GlobalDefinition::new("site_settings");
        g.labels = Labels {
            singular: Some(LocalizedString::Localized(labels)),
            plural: None,
        };
        assert_eq!(g.display_name_for("de", "en"), "Seiteneinstellungen");
        assert_eq!(g.display_name_for("fr", "en"), "Site Settings");
    }

    #[test]
    fn global_display_name_for_falls_back_to_slug() {
        let g = make_global("site_settings", None);
        assert_eq!(g.display_name_for("de", "en"), "site_settings");
    }

    #[test]
    fn global_has_versions_true() {
        let mut g = make_global("site_settings", None);
        g.versions = Some(VersionsConfig::new(false, 0));
        assert!(g.has_versions());
    }

    #[test]
    fn global_has_versions_false() {
        let g = make_global("site_settings", None);
        assert!(!g.has_versions());
    }

    #[test]
    fn global_has_drafts_true() {
        let mut g = make_global("site_settings", None);
        g.versions = Some(VersionsConfig::new(true, 0));
        assert!(g.has_drafts());
    }

    #[test]
    fn global_has_drafts_false_no_versions() {
        let g = make_global("site_settings", None);
        assert!(!g.has_drafts());
    }

    #[test]
    fn global_has_drafts_false_drafts_disabled() {
        let mut g = make_global("site_settings", None);
        g.versions = Some(VersionsConfig::new(false, 0));
        assert!(!g.has_drafts());
    }
}
