use serde::{Deserialize, Serialize};
use super::field::FieldDefinition;
use super::upload::CollectionUpload;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStrategy {
    pub name: String,
    /// Lua function ref (module.function format)
    pub authenticate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionAuth {
    pub enabled: bool,
    #[serde(default = "default_token_expiry")]
    pub token_expiry: u64,
    #[serde(default)]
    pub strategies: Vec<AuthStrategy>,
    #[serde(default)]
    pub disable_local: bool,
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionLabels {
    #[serde(default)]
    pub singular: Option<String>,
    #[serde(default)]
    pub plural: Option<String>,
}

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
}

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
}

fn default_true() -> bool {
    true
}

impl CollectionDefinition {
    /// Get the display label (plural form, falls back to slug).
    pub fn display_name(&self) -> &str {
        self.labels.plural.as_deref()
            .unwrap_or(&self.slug)
    }

    /// Get the singular label (falls back to slug).
    pub fn singular_name(&self) -> &str {
        self.labels.singular.as_deref()
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
}

impl GlobalDefinition {
    /// Get the display label (singular, falls back to slug).
    pub fn display_name(&self) -> &str {
        self.labels.singular.as_deref()
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
                singular: singular.map(|s| s.to_string()),
                plural: plural.map(|s| s.to_string()),
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
}
