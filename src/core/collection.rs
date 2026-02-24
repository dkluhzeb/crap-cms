//! Collection and global definition types parsed from Lua configuration files.

use serde::{Deserialize, Serialize};
use super::field::{FieldDefinition, LocalizedString};
use super::upload::CollectionUpload;

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

/// Controls live event broadcasting for a collection or global.
/// `None` = enabled (broadcast all events).
/// `Some(LiveSetting::Disabled)` = never broadcast.
/// `Some(LiveSetting::Function(ref))` = Lua function decides per-event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LiveSetting {
    Disabled,
    Function(String),
}

/// Lua function references for collection-level access control (read/create/update/delete).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionAccess {
    #[serde(default)]
    pub read: Option<String>,
    #[serde(default)]
    pub create: Option<String>,
    #[serde(default)]
    pub update: Option<String>,
    #[serde(default)]
    pub delete: Option<String>,
}

/// A custom authentication strategy (name + Lua function reference).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStrategy {
    pub name: String,
    /// Lua function ref (module.function format)
    pub authenticate: String,
}

/// Authentication configuration for a collection (JWT, strategies, local login).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionAuth {
    pub enabled: bool,
    #[serde(default = "default_token_expiry")]
    pub token_expiry: u64,
    #[serde(default)]
    pub strategies: Vec<AuthStrategy>,
    #[serde(default)]
    pub disable_local: bool,
    /// Enable email verification requirement for new users. Default: false.
    #[serde(default)]
    pub verify_email: bool,
    /// Enable forgot password flow. Default: true (when auth enabled).
    #[serde(default = "default_true_auth")]
    pub forgot_password: bool,
}

fn default_true_auth() -> bool {
    true
}

fn default_token_expiry() -> u64 {
    7200
}

impl Default for CollectionAuth {
    fn default() -> Self {
        Self {
            enabled: false,
            token_expiry: default_token_expiry(),
            strategies: Vec::new(),
            disable_local: false,
            verify_email: false,
            forgot_password: true,
        }
    }
}

/// Human-readable singular/plural labels for the admin UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionLabels {
    #[serde(default)]
    pub singular: Option<LocalizedString>,
    #[serde(default)]
    pub plural: Option<LocalizedString>,
}

/// Admin UI display options (title field, default sort, visibility, searchable fields).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionAdmin {
    #[serde(default)]
    pub use_as_title: Option<String>,
    #[serde(default)]
    pub default_sort: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub list_searchable_fields: Vec<String>,
}

/// Lua function references for collection-level lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionHooks {
    #[serde(default)]
    pub before_validate: Vec<String>,
    #[serde(default)]
    pub before_change: Vec<String>,
    #[serde(default)]
    pub after_change: Vec<String>,
    #[serde(default)]
    pub before_read: Vec<String>,
    #[serde(default)]
    pub after_read: Vec<String>,
    #[serde(default)]
    pub before_delete: Vec<String>,
    #[serde(default)]
    pub after_delete: Vec<String>,
    #[serde(default)]
    pub before_broadcast: Vec<String>,
}

/// Full definition of a collection, parsed from a Lua file. Maps to one SQLite table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionDefinition {
    pub slug: String,
    #[serde(default)]
    pub labels: CollectionLabels,
    #[serde(default = "default_true")]
    pub timestamps: bool,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    #[serde(default)]
    pub admin: CollectionAdmin,
    #[serde(default)]
    pub hooks: CollectionHooks,
    #[serde(default)]
    pub auth: Option<CollectionAuth>,
    #[serde(default)]
    pub upload: Option<CollectionUpload>,
    #[serde(default)]
    pub access: CollectionAccess,
    #[serde(default)]
    pub live: Option<LiveSetting>,
    #[serde(default)]
    pub versions: Option<VersionsConfig>,
}

fn default_true() -> bool {
    true
}

impl CollectionDefinition {
    /// Get the display label (plural form, falls back to slug). Uses default resolution.
    pub fn display_name(&self) -> &str {
        self.labels.plural.as_ref()
            .map(|ls| ls.resolve_default())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the singular label (falls back to slug). Uses default resolution.
    pub fn singular_name(&self) -> &str {
        self.labels.singular.as_ref()
            .map(|ls| ls.resolve_default())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the display label resolved for a specific locale.
    #[allow(dead_code)]
    pub fn display_name_for(&self, locale: &str, default_locale: &str) -> &str {
        self.labels.plural.as_ref()
            .map(|ls| ls.resolve(locale, default_locale))
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the singular label resolved for a specific locale.
    #[allow(dead_code)]
    pub fn singular_name_for(&self, locale: &str, default_locale: &str) -> &str {
        self.labels.singular.as_ref()
            .map(|ls| ls.resolve(locale, default_locale))
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the field to use as item title in admin lists.
    pub fn title_field(&self) -> Option<&str> {
        self.admin.use_as_title.as_deref()
    }

    /// Check if this collection has auth enabled.
    pub fn is_auth_collection(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.enabled)
    }

    /// Check if this collection is an upload collection.
    pub fn is_upload_collection(&self) -> bool {
        self.upload.as_ref().is_some_and(|u| u.enabled)
    }

    /// Check if this collection has versioning enabled.
    pub fn has_versions(&self) -> bool {
        self.versions.is_some()
    }

    /// Check if this collection has drafts enabled (versioning + drafts flag).
    pub fn has_drafts(&self) -> bool {
        self.versions.as_ref().is_some_and(|v| v.drafts)
    }
}

/// Global definitions are simpler — single-document collections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalDefinition {
    pub slug: String,
    #[serde(default)]
    pub labels: CollectionLabels,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    #[serde(default)]
    pub hooks: CollectionHooks,
    #[serde(default)]
    pub access: CollectionAccess,
    #[serde(default)]
    pub live: Option<LiveSetting>,
}

impl GlobalDefinition {
    /// Get the display label (singular, falls back to slug). Uses default resolution.
    pub fn display_name(&self) -> &str {
        self.labels.singular.as_ref()
            .map(|ls| ls.resolve_default())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }

    /// Get the display label resolved for a specific locale.
    #[allow(dead_code)]
    pub fn display_name_for(&self, locale: &str, default_locale: &str) -> &str {
        self.labels.singular.as_ref()
            .map(|ls| ls.resolve(locale, default_locale))
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_collection(slug: &str, singular: Option<&str>, plural: Option<&str>, title_field: Option<&str>) -> CollectionDefinition {
        CollectionDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels {
                singular: singular.map(|s| LocalizedString::Plain(s.to_string())),
                plural: plural.map(|s| LocalizedString::Plain(s.to_string())),
            },
            timestamps: true,
            fields: Vec::new(),
            admin: CollectionAdmin {
                use_as_title: title_field.map(|s| s.to_string()),
                ..Default::default()
            },
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        }
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

    // ── is_auth_collection / is_upload_collection tests ─────────────────────

    #[test]
    fn is_auth_collection_true() {
        let mut col = make_collection("users", None, None, None);
        col.auth = Some(CollectionAuth { enabled: true, ..Default::default() });
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
        col.auth = Some(CollectionAuth { enabled: false, ..Default::default() });
        assert!(!col.is_auth_collection(), "auth.enabled=false = not auth");
    }

    #[test]
    fn is_upload_collection() {
        use crate::core::upload::CollectionUpload;
        let mut col = make_collection("media", None, None, None);
        col.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });
        assert!(col.is_upload_collection());
    }

    #[test]
    fn is_upload_collection_false_default() {
        let col = make_collection("posts", None, None, None);
        assert!(!col.is_upload_collection());
    }
}
