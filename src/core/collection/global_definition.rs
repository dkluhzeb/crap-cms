//! Global definitions — single-document collections.

use crate::core::{
    FieldDefinition, Slug,
    collection::{
        Access, Hooks, Labels, LiveMode, LiveSetting, McpConfig, VersionsConfig,
        labels::resolve_label,
    },
};
use crate::typegen::lua::LuaAnnotation;
use serde::{Deserialize, Serialize};

/// Global definitions are simpler — single-document collections.
#[derive(Debug, Clone, Default, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.GlobalConfig")]
pub struct GlobalDefinition {
    /// Unique identifier for the global.
    //
    // Not part of the Lua config table — passed as the first arg to
    // `crap.globals.define(slug, config)`.
    #[lua(skip)]
    pub slug: Slug,
    /// Display names.
    #[serde(default)]
    #[lua(optional)]
    pub labels: Labels,
    /// Field definitions.
    #[serde(default)]
    #[lua(ty = "crap.FieldDefinition[]", optional)]
    pub fields: Vec<FieldDefinition>,
    /// Hook references.
    #[serde(default)]
    #[lua(optional)]
    pub hooks: Hooks,
    /// Access control function refs. Globals support only `read`, `draft`,
    /// `update`, and the `versions` toggle — `create`/`delete`/`trash` are
    /// rejected at config load (a global is a single row with `get`/`update`
    /// operations only).
    #[serde(default)]
    #[lua(ty = "crap.GlobalAccess", optional)]
    pub access: Access,
    /// MCP tool description and options.
    #[serde(default)]
    #[lua(optional)]
    pub mcp: McpConfig,
    /// Live event broadcasting. Same as collection `live`.
    #[serde(default)]
    #[lua(ty = "boolean | string", optional)]
    pub live: Option<LiveSetting>,
    /// Controls what data events carry (metadata-only or full with `after_read` hooks).
    //
    // Internal-only — see the parallel field on `CollectionDefinition`.
    #[serde(default)]
    #[lua(skip)]
    pub live_mode: LiveMode,
    /// Enable versioning. Same as collection `versions`.
    #[serde(default)]
    #[lua(ty = "boolean | crap.VersionsConfig", optional)]
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

    /// Whether this global silently exposes its unpublished (draft) content to
    /// *everyone* (including unauthenticated callers) when `default_deny` is
    /// false: drafts are enabled but neither `access.draft` nor its `access.update`
    /// fallback is set, so a hook-less draft view resolves to "allowed for all".
    /// Globals have no soft-delete, so `draft` is the only lifecycle view that can
    /// leak this way. `read` (published) is intentionally excluded — published
    /// globals being world-readable is the expected default. Mirrors
    /// [`CollectionDefinition::publicly_exposed_lifecycle_views`]; returns false
    /// when `default_deny` is true.
    #[must_use]
    pub fn draft_view_publicly_exposed(&self, default_deny: bool) -> bool {
        !default_deny && self.has_drafts() && self.access.resolve_draft().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{HookRef, LocalizedString};

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

    #[test]
    fn draft_view_publicly_exposed_when_drafts_on_and_ungated() {
        let mut g = make_global("site_settings", None);
        g.versions = Some(VersionsConfig::new(true, 0));

        // default_deny=false, drafts on, no draft/update key → draft world-readable.
        assert!(g.draft_view_publicly_exposed(false));

        // default_deny=true closes the view regardless of keys.
        assert!(!g.draft_view_publicly_exposed(true));
    }

    #[test]
    fn draft_view_not_exposed_when_gated_or_features_off() {
        let mut g = make_global("site_settings", None);
        g.versions = Some(VersionsConfig::new(true, 0));

        // An `update` rule covers the draft view via the fallback → not public.
        g.access.update = Some(HookRef::new("access.editors"));
        assert!(!g.draft_view_publicly_exposed(false));

        // An explicit `draft` key also gates it.
        g.access.update = None;
        g.access.draft = Some(HookRef::new("access.editors"));
        assert!(!g.draft_view_publicly_exposed(false));

        // Drafts disabled → no draft view exists to expose.
        let g = make_global("site_settings", None);
        assert!(!g.draft_view_publicly_exposed(false));
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
