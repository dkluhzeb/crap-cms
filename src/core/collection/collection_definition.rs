//! Full definition of a collection, parsed from a Lua file. Maps to one SQLite table.

use super::{
    Access, AdminConfig, Auth, Hooks, IndexDefinition, Labels, LiveSetting, McpConfig,
    VersionsConfig,
};
use crate::core::field::FieldDefinition;
use crate::core::upload::CollectionUpload;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// Full definition of a collection, parsed from a Lua file. Maps to one SQLite table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionDefinition {
    /// Unique identifier for the collection, used in URLs and database table names.
    pub slug: String,
    /// Human-readable labels for the collection (singular and plural).
    #[serde(default)]
    pub labels: Labels,
    /// Whether to automatically manage `created_at` and `updated_at` timestamps.
    #[serde(default = "default_true")]
    pub timestamps: bool,
    /// List of fields that make up the collection's schema.
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    /// Configuration for how this collection appears and behaves in the admin UI.
    #[serde(default)]
    pub admin: AdminConfig,
    /// Lua hook functions triggered during various lifecycle events.
    #[serde(default)]
    pub hooks: Hooks,
    /// Authentication settings, if this collection is used for user management.
    #[serde(default)]
    pub auth: Option<Auth>,
    /// File upload configuration, if this collection supports media/attachments.
    #[serde(default)]
    pub upload: Option<CollectionUpload>,
    /// Access control rules for reading, creating, updating, and deleting items.
    #[serde(default)]
    pub access: Access,
    /// Model Context Protocol (MCP) configuration for AI integration.
    #[serde(default)]
    pub mcp: McpConfig,
    /// Real-time update settings for this collection.
    #[serde(default)]
    pub live: Option<LiveSetting>,
    /// Versioning and draft configuration.
    #[serde(default)]
    pub versions: Option<VersionsConfig>,
    /// Custom database indexes to optimize query performance.
    #[serde(default)]
    pub indexes: Vec<IndexDefinition>,
}

impl Default for CollectionDefinition {
    fn default() -> Self {
        Self {
            slug: String::new(),
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
            versions: None,
            indexes: Vec::new(),
        }
    }
}

impl CollectionDefinition {
    /// Create a new `CollectionDefinition` with the given slug and default settings.
    pub fn new(slug: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            ..Default::default()
        }
    }

    /// Create a builder for `CollectionDefinition`.
    pub fn builder(slug: impl Into<String>) -> super::CollectionDefinitionBuilder {
        super::CollectionDefinitionBuilder::new(slug)
    }

    /// Get the display label (plural form, falls back to slug). Uses default resolution.
    pub fn display_name(&self) -> &str {
        self.labels
            .plural
            .as_ref()
            .map(|ls| ls.resolve_default())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the singular label (falls back to slug). Uses default resolution.
    pub fn singular_name(&self) -> &str {
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
            .plural
            .as_ref()
            .map(|ls| ls.resolve(locale, default_locale))
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the singular label resolved for a specific locale.
    #[allow(dead_code)]
    pub fn singular_name_for(&self, locale: &str, default_locale: &str) -> &str {
        self.labels
            .singular
            .as_ref()
            .map(|ls| ls.resolve(locale, default_locale))
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the field name to use as item title in admin lists.
    pub fn title_field(&self) -> Option<&str> {
        self.admin.use_as_title.as_deref()
    }

    /// Check if this collection has authentication enabled.
    pub fn is_auth_collection(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.enabled)
    }

    /// Check if this collection has file upload support enabled.
    pub fn is_upload_collection(&self) -> bool {
        self.upload.as_ref().is_some_and(|u| u.enabled)
    }

    /// Check if this collection has versioning enabled.
    pub fn has_versions(&self) -> bool {
        self.versions.is_some()
    }

    /// Check if this collection has drafts enabled (versioning with drafts flag).
    pub fn has_drafts(&self) -> bool {
        self.versions.as_ref().is_some_and(|v| v.drafts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::LocalizedString;
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
            use_as_title: title_field.map(|s| s.to_string()),
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
        use crate::core::upload::CollectionUpload;
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
        use crate::core::upload::CollectionUpload;
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
}
