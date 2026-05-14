//! Global definitions — single-document collections.

use crate::core::{
    FieldDefinition, Slug,
    collection::{
        Access, Hooks, Labels, LiveMode, LiveSetting, McpConfig, VersionsConfig,
        labels::resolve_label,
    },
};
use serde::{Deserialize, Serialize};

/// Global definitions are simpler — single-document collections.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Controls what data events carry (metadata-only or full with `after_read` hooks).
    #[serde(default)]
    pub live_mode: LiveMode,
    /// Versioning and draft configuration.
    #[serde(default)]
    pub versions: Option<VersionsConfig>,
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
    #[must_use]
    pub fn display_name(&self) -> &str {
        resolve_label(self.labels.singular.as_ref(), &self.slug, None)
    }

    /// Get the display label resolved for a specific locale.
    #[must_use]
    pub fn display_name_for(&self, locale: &str, default_locale: &str) -> &str {
        resolve_label(
            self.labels.singular.as_ref(),
            &self.slug,
            Some((locale, default_locale)),
        )
    }

    /// Check if this global has versioning enabled.
    #[must_use]
    pub fn has_versions(&self) -> bool {
        self.versions.is_some()
    }

    /// Check if this global has drafts enabled (versioning with drafts flag).
    #[must_use]
    pub fn has_drafts(&self) -> bool {
        self.versions.as_ref().is_some_and(|v| v.drafts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LocalizedString;
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

/// Builder for [`GlobalDefinition`].
///
/// `slug` is taken in `new()`. All other fields default via
/// [`GlobalDefinition::default()`].
pub struct GlobalDefinitionBuilder {
    inner: GlobalDefinition,
}

impl GlobalDefinitionBuilder {
    /// Create a new builder for a global with the given slug.
    pub fn new(slug: impl Into<Slug>) -> Self {
        Self {
            inner: GlobalDefinition {
                slug: slug.into(),
                ..Default::default()
            },
        }
    }

    /// Set localized labels for the global.
    #[must_use]
    pub fn labels(mut self, v: Labels) -> Self {
        self.inner.labels = v;
        self
    }

    /// Set the fields for this global.
    #[must_use]
    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    /// Set the hooks for this global.
    #[must_use]
    pub fn hooks(mut self, v: Hooks) -> Self {
        self.inner.hooks = v;
        self
    }

    /// Set access control configuration for this global.
    #[must_use]
    pub fn access(mut self, v: Access) -> Self {
        self.inner.access = v;
        self
    }

    /// Set MCP (Model Context Protocol) configuration for this global.
    #[must_use]
    pub fn mcp(mut self, v: McpConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    /// Set live update settings for this global.
    #[must_use]
    pub fn live(mut self, v: LiveSetting) -> Self {
        self.inner.live = Some(v);
        self
    }

    /// Enable and configure versioning/drafts for this global.
    #[must_use]
    pub fn versions(mut self, v: VersionsConfig) -> Self {
        self.inner.versions = Some(v);
        self
    }

    /// Build the final `GlobalDefinition`.
    #[must_use]
    pub fn build(self) -> GlobalDefinition {
        self.inner
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;

    #[test]
    fn builds_with_defaults() {
        let def = GlobalDefinitionBuilder::new("site_settings").build();
        assert_eq!(def.slug, "site_settings");
        assert!(def.fields.is_empty());
        assert!(def.versions.is_none());
    }

    #[test]
    fn builds_with_overrides() {
        let def = GlobalDefinitionBuilder::new("site_settings")
            .versions(VersionsConfig::new(true, 0))
            .build();
        assert!(def.has_drafts());
    }
}
