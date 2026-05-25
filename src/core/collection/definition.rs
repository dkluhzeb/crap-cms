//! Full definition of a collection, parsed from a Lua file. Maps to one `SQLite` table.

use serde::{Deserialize, Serialize};

use crate::core::{
    FieldDefinition, Slug,
    collection::{
        Access, AdminConfig, Auth, Hooks, IndexDefinition, Labels, LiveMode, LiveSetting,
        McpConfig, VersionsConfig, labels::resolve_label,
    },
    upload::CollectionUpload,
};
use crate::typegen::lua::LuaAnnotation;

fn default_true() -> bool {
    true
}

/// Full definition of a collection, parsed from a Lua file. Maps to one `SQLite` table.
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.CollectionConfig")]
pub struct CollectionDefinition {
    /// Unique identifier for the collection, used in URLs and database table names.
    //
    // Not part of the Lua config table — passed as the first arg to
    // `crap.collections.define(slug, config)`.
    #[lua(skip)]
    pub slug: Slug,
    /// Display names.
    #[serde(default)]
    #[lua(optional)]
    pub labels: Labels,
    /// Auto `created_at`/`updated_at` (default: true).
    #[serde(default = "default_true")]
    #[lua(optional)]
    pub timestamps: bool,
    /// Field definitions.
    #[serde(default)]
    #[lua(ty = "crap.FieldDefinition[]", optional)]
    pub fields: Vec<FieldDefinition>,
    /// Admin UI options.
    #[serde(default)]
    #[lua(optional)]
    pub admin: AdminConfig,
    /// Hook references.
    #[serde(default)]
    #[lua(optional)]
    pub hooks: Hooks,
    /// Enable authentication on this collection. `true` for defaults, or a config table with `strategies`/`token_expiry`/`disable_local`.
    #[serde(default)]
    #[lua(ty = "boolean | crap.Auth", optional)]
    pub auth: Option<Auth>,
    /// Enable file uploads. `true` for defaults, or a config table with `mime_types`/`max_file_size`/`image_sizes`.
    #[serde(default)]
    #[lua(ty = "boolean | crap.CollectionUpload", optional)]
    pub upload: Option<CollectionUpload>,
    /// Access control function refs.
    #[serde(default)]
    #[lua(optional)]
    pub access: Access,
    /// MCP tool description and options.
    #[serde(default)]
    #[lua(optional)]
    pub mcp: McpConfig,
    /// Live event broadcasting. `false` to disable, string for Lua function ref that receives `{ collection, operation, data }` and returns boolean. Absent = enabled (broadcast all).
    #[serde(default)]
    #[lua(ty = "boolean | string", optional)]
    pub live: Option<LiveSetting>,
    /// Controls what data events carry (metadata-only or full with `after_read` hooks).
    //
    // Internal-only: Lua's `live` field controls both surfaces via the
    // union shape above — `live_mode` is set independently from Rust.
    #[serde(default)]
    #[lua(skip)]
    pub live_mode: LiveMode,
    /// Enable versioning. `true` for defaults, or a config table with `drafts`/`max_versions`.
    #[serde(default)]
    #[lua(ty = "boolean | crap.VersionsConfig", optional)]
    pub versions: Option<VersionsConfig>,
    /// Compound indexes (multi-column). Created on startup, stale indexes dropped.
    #[serde(default)]
    #[lua(optional)]
    pub indexes: Vec<IndexDefinition>,
    /// Enable soft deletes (move to trash instead of permanent deletion).
    /// When `true`, `delete` moves documents to trash; the row is purged
    /// later according to `soft_delete_retention`.
    #[serde(default)]
    #[lua(optional)]
    pub soft_delete: bool,
    /// Retention period for soft-deleted documents before auto-purge.
    /// Human-readable duration: `"30d"`, `"7d"`, `"90d"`. `nil` = manual
    /// purge only. Only relevant when `soft_delete = true`.
    #[serde(default)]
    #[lua(optional)]
    pub soft_delete_retention: Option<String>,
}

impl Default for CollectionDefinition {
    fn default() -> Self {
        Self {
            slug: Slug::new(""),
            labels: Labels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: AdminConfig::default(),
            hooks: Hooks::default(),
            auth: None,
            upload: None,
            access: Access::default(),
            mcp: McpConfig::default(),
            live: None,
            live_mode: LiveMode::default(),
            versions: None,
            indexes: Vec::new(),
            soft_delete: false,
            soft_delete_retention: None,
        }
    }
}

impl CollectionDefinition {
    /// Create a new `CollectionDefinition` with the given slug and default settings.
    pub fn new(slug: impl Into<Slug>) -> Self {
        Self {
            slug: slug.into(),
            ..Default::default()
        }
    }

    /// Create a builder for `CollectionDefinition`.
    pub fn builder(slug: impl Into<Slug>) -> super::CollectionDefinitionBuilder {
        super::CollectionDefinitionBuilder::new(slug)
    }

    /// Get the display label (plural form, falls back to slug). Uses default resolution.
    #[must_use]
    pub fn display_name(&self) -> &str {
        resolve_label(self.labels.plural.as_ref(), &self.slug, None)
    }

    /// Get the singular label (falls back to slug). Uses default resolution.
    #[must_use]
    pub fn singular_name(&self) -> &str {
        resolve_label(self.labels.singular.as_ref(), &self.slug, None)
    }

    /// Get the display label resolved for a specific locale.
    #[must_use]
    pub fn display_name_for(&self, locale: &str, default_locale: &str) -> &str {
        resolve_label(
            self.labels.plural.as_ref(),
            &self.slug,
            Some((locale, default_locale)),
        )
    }

    /// Get the singular label resolved for a specific locale.
    #[must_use]
    pub fn singular_name_for(&self, locale: &str, default_locale: &str) -> &str {
        resolve_label(
            self.labels.singular.as_ref(),
            &self.slug,
            Some((locale, default_locale)),
        )
    }

    /// Get the field name to use as item title in admin lists.
    #[must_use]
    pub fn title_field(&self) -> Option<&str> {
        self.admin.use_as_title.as_deref()
    }

    /// Check if this collection has authentication enabled.
    #[must_use]
    pub fn is_auth_collection(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.enabled)
    }

    /// Check if this collection has file upload support enabled.
    #[must_use]
    pub fn is_upload_collection(&self) -> bool {
        self.upload.as_ref().is_some_and(|u| u.enabled)
    }

    /// Check if this collection has versioning enabled.
    #[must_use]
    pub fn has_versions(&self) -> bool {
        self.versions.is_some()
    }

    /// Check if this collection has drafts enabled (versioning with drafts flag).
    #[must_use]
    pub fn has_drafts(&self) -> bool {
        self.versions.as_ref().is_some_and(|v| v.drafts)
    }

    /// Check if this collection has soft deletes enabled.
    #[must_use]
    pub fn has_soft_delete(&self) -> bool {
        self.soft_delete
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{LocalizedString, upload::CollectionUpload};
    use std::collections::HashMap;

    fn make_collection(
        slug: &str,
        singular: Option<&str>,
        plural: Option<&str>,
        title_field: Option<&str>,
    ) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.labels = Labels {
            singular: singular.map(|s| LocalizedString::Plain(s.to_string())),
            plural: plural.map(|s| LocalizedString::Plain(s.to_string())),
        };
        def.admin = AdminConfig {
            use_as_title: title_field.map(std::string::ToString::to_string),
            ..Default::default()
        };
        def
    }

    #[test]
    fn display_name_uses_plural_label() {
        let col = make_collection("posts", Some("Post"), Some("Posts"), None);
        assert_eq!(col.display_name(), "Posts");
    }

    #[test]
    fn display_name_falls_back_to_slug() {
        let col = make_collection("posts", None, None, None);
        assert_eq!(col.display_name(), "posts");
    }

    #[test]
    fn singular_name_uses_label() {
        let col = make_collection("posts", Some("Post"), Some("Posts"), None);
        assert_eq!(col.singular_name(), "Post");
    }

    #[test]
    fn singular_name_falls_back_to_slug() {
        let col = make_collection("posts", None, None, None);
        assert_eq!(col.singular_name(), "posts");
    }

    #[test]
    fn title_field_returns_configured_value() {
        let col = make_collection("posts", None, None, Some("title"));
        assert_eq!(col.title_field(), Some("title"));
    }

    #[test]
    fn title_field_returns_none_when_not_set() {
        let col = make_collection("posts", None, None, None);
        assert_eq!(col.title_field(), None);
    }

    #[test]
    fn is_auth_collection_true() {
        let mut col = make_collection("users", None, None, None);
        col.auth = Some(Auth::new(true));
        assert!(col.is_auth_collection());
    }

    #[test]
    fn is_auth_collection_false_default() {
        let col = make_collection("posts", None, None, None);
        assert!(!col.is_auth_collection(), "no auth config = not auth");
    }

    #[test]
    fn is_auth_collection_false_when_disabled() {
        let mut col = make_collection("users", None, None, None);
        col.auth = Some(Auth::new(false));
        assert!(!col.is_auth_collection(), "auth.enabled=false = not auth");
    }

    #[test]
    fn is_upload_collection() {
        let mut col = make_collection("media", None, None, None);
        col.upload = Some(CollectionUpload::new());
        assert!(col.is_upload_collection());
    }

    #[test]
    fn is_upload_collection_false_default() {
        let col = make_collection("posts", None, None, None);
        assert!(!col.is_upload_collection());
    }

    #[test]
    fn has_versions_true() {
        let mut col = make_collection("posts", None, None, None);
        col.versions = Some(VersionsConfig::new(false, 0));
        assert!(col.has_versions());
    }

    #[test]
    fn has_versions_false_default() {
        let col = make_collection("posts", None, None, None);
        assert!(!col.has_versions());
    }

    #[test]
    fn has_drafts_true() {
        let mut col = make_collection("posts", None, None, None);
        col.versions = Some(VersionsConfig::new(true, 5));
        assert!(col.has_drafts());
    }

    #[test]
    fn has_drafts_false_when_no_versions() {
        let col = make_collection("posts", None, None, None);
        assert!(!col.has_drafts());
    }

    #[test]
    fn has_drafts_false_when_versions_but_no_drafts() {
        let mut col = make_collection("posts", None, None, None);
        col.versions = Some(VersionsConfig::new(false, 10));
        assert!(!col.has_drafts());
    }

    fn make_localized_collection() -> CollectionDefinition {
        let mut labels = HashMap::new();
        labels.insert("en".to_string(), "Posts".to_string());
        labels.insert("de".to_string(), "Beiträge".to_string());

        let mut singular_labels = HashMap::new();
        singular_labels.insert("en".to_string(), "Post".to_string());
        singular_labels.insert("de".to_string(), "Beitrag".to_string());

        let mut def = CollectionDefinition::new("posts");
        def.labels = Labels {
            singular: Some(LocalizedString::Localized(singular_labels)),
            plural: Some(LocalizedString::Localized(labels)),
        };
        def
    }

    #[test]
    fn display_name_for_returns_locale() {
        let col = make_localized_collection();
        assert_eq!(col.display_name_for("de", "en"), "Beiträge");
    }

    #[test]
    fn display_name_for_falls_back_to_default_locale() {
        let col = make_localized_collection();
        assert_eq!(col.display_name_for("fr", "en"), "Posts");
    }

    #[test]
    fn display_name_for_falls_back_to_slug() {
        let col = make_collection("posts", None, None, None);
        assert_eq!(col.display_name_for("de", "en"), "posts");
    }

    #[test]
    fn singular_name_for_returns_locale() {
        let col = make_localized_collection();
        assert_eq!(col.singular_name_for("de", "en"), "Beitrag");
    }

    #[test]
    fn singular_name_for_falls_back_to_default_locale() {
        let col = make_localized_collection();
        assert_eq!(col.singular_name_for("fr", "en"), "Post");
    }

    #[test]
    fn singular_name_for_falls_back_to_slug() {
        let col = make_collection("posts", None, None, None);
        assert_eq!(col.singular_name_for("de", "en"), "posts");
    }

    #[test]
    fn display_name_empty_string_falls_back_to_slug() {
        let col = make_collection("posts", None, Some(""), None);
        assert_eq!(col.display_name(), "posts");
    }

    #[test]
    fn singular_name_empty_string_falls_back_to_slug() {
        let col = make_collection("posts", Some(""), None, None);
        assert_eq!(col.singular_name(), "posts");
    }

    #[test]
    fn display_name_for_empty_localized_falls_back_to_slug() {
        let labels = HashMap::new();
        let mut col = CollectionDefinition::new("posts");
        col.labels = Labels {
            singular: None,
            plural: Some(LocalizedString::Localized(labels)),
        };
        assert_eq!(col.display_name_for("en", "en"), "posts");
    }

    #[test]
    fn is_upload_collection_false_when_disabled() {
        let mut col = make_collection("media", None, None, None);
        col.upload = Some(CollectionUpload::default());
        assert!(!col.is_upload_collection());
    }

    #[test]
    fn live_setting_disabled() {
        let mut col = make_collection("posts", None, None, None);
        col.live = Some(LiveSetting::Disabled);
        assert!(matches!(col.live, Some(LiveSetting::Disabled)));
    }

    #[test]
    fn live_setting_function() {
        let mut col = make_collection("posts", None, None, None);
        col.live = Some(LiveSetting::Function("hooks.live_filter".to_string()));
        match &col.live {
            Some(LiveSetting::Function(s)) => assert_eq!(s, "hooks.live_filter"),
            _ => panic!("expected LiveSetting::Function"),
        }
    }

    #[test]
    fn versions_config_defaults() {
        let v = VersionsConfig::new(false, 0);
        assert!(!v.drafts);
        assert_eq!(v.max_versions, 0);
    }

    #[test]
    fn soft_delete_defaults_to_false() {
        let col = make_collection("posts", None, None, None);
        assert!(!col.has_soft_delete());
        assert!(col.soft_delete_retention.is_none());
    }

    #[test]
    fn soft_delete_enabled() {
        let mut col = make_collection("posts", None, None, None);
        col.soft_delete = true;
        col.soft_delete_retention = Some("30d".to_string());
        assert!(col.has_soft_delete());
        assert_eq!(col.soft_delete_retention.as_deref(), Some("30d"));
    }
}

/// Builder for [`CollectionDefinition`].
///
/// `slug` is taken in `new()`. All other fields default via
/// [`CollectionDefinition::default()`].
pub struct CollectionDefinitionBuilder {
    inner: CollectionDefinition,
}

impl CollectionDefinitionBuilder {
    /// Create a new `CollectionDefinitionBuilder` with the given slug.
    pub fn new(slug: impl Into<Slug>) -> Self {
        Self {
            inner: CollectionDefinition {
                slug: slug.into(),
                ..Default::default()
            },
        }
    }

    /// Set the plural and singular labels for the collection.
    #[must_use]
    pub fn labels(mut self, v: Labels) -> Self {
        self.inner.labels = v;
        self
    }

    /// Set whether the collection should include standard timestamps.
    #[must_use]
    pub fn timestamps(mut self, v: bool) -> Self {
        self.inner.timestamps = v;
        self
    }

    /// Set the field definitions for the collection.
    #[must_use]
    pub fn fields(mut self, v: Vec<FieldDefinition>) -> Self {
        self.inner.fields = v;
        self
    }

    /// Set the admin UI configuration for the collection.
    #[must_use]
    pub fn admin(mut self, v: AdminConfig) -> Self {
        self.inner.admin = v;
        self
    }

    /// Set the lifecycle hooks for the collection.
    #[must_use]
    pub fn hooks(mut self, v: Hooks) -> Self {
        self.inner.hooks = v;
        self
    }

    /// Set the authentication configuration for the collection.
    #[must_use]
    pub fn auth(mut self, v: Auth) -> Self {
        self.inner.auth = Some(v);
        self
    }

    /// Set the file upload configuration for the collection.
    #[must_use]
    pub fn upload(mut self, v: CollectionUpload) -> Self {
        self.inner.upload = Some(v);
        self
    }

    /// Set the access control rules for the collection.
    #[must_use]
    pub fn access(mut self, v: Access) -> Self {
        self.inner.access = v;
        self
    }

    /// Set the MCP-specific configuration for the collection.
    #[must_use]
    pub fn mcp(mut self, v: McpConfig) -> Self {
        self.inner.mcp = v;
        self
    }

    /// Set the live update settings for the collection.
    #[must_use]
    pub fn live(mut self, v: LiveSetting) -> Self {
        self.inner.live = Some(v);
        self
    }

    /// Set the versioning and drafts configuration for the collection.
    #[must_use]
    pub fn versions(mut self, v: VersionsConfig) -> Self {
        self.inner.versions = Some(v);
        self
    }

    /// Set additional database indexes for the collection.
    #[must_use]
    pub fn indexes(mut self, v: Vec<IndexDefinition>) -> Self {
        self.inner.indexes = v;
        self
    }

    /// Enable soft deletes for the collection.
    #[must_use]
    pub fn soft_delete(mut self, v: bool) -> Self {
        self.inner.soft_delete = v;
        self
    }

    /// Set the retention period for soft-deleted documents.
    #[must_use]
    pub fn soft_delete_retention(mut self, v: impl Into<String>) -> Self {
        self.inner.soft_delete_retention = Some(v.into());
        self
    }

    /// Build the final `CollectionDefinition` instance.
    #[must_use]
    pub fn build(self) -> CollectionDefinition {
        self.inner
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;

    #[test]
    fn builds_with_defaults() {
        let def = CollectionDefinitionBuilder::new("posts").build();
        assert_eq!(def.slug, "posts");
        assert!(def.timestamps);
        assert!(def.fields.is_empty());
        assert!(def.auth.is_none());
        assert!(def.upload.is_none());
        assert!(def.versions.is_none());
        assert!(def.indexes.is_empty());
    }

    #[test]
    fn builds_with_overrides() {
        let def = CollectionDefinitionBuilder::new("posts")
            .timestamps(false)
            .versions(VersionsConfig::new(true, 5))
            .build();
        assert_eq!(def.slug, "posts");
        assert!(!def.timestamps);
        assert!(def.versions.is_some());
        assert!(def.versions.unwrap().drafts);
    }

    #[test]
    fn builds_with_auth() {
        let def = CollectionDefinitionBuilder::new("users")
            .auth(Auth::new(true))
            .build();
        assert!(def.is_auth_collection());
    }
}
