//! Centralized template context builder for all admin handlers.
//!
//! Every admin page receives a structured context built through `ContextBuilder`.
//! This replaces the ad-hoc `serde_json::json!()` calls in individual handlers.

pub use crate::admin::ContextBuilder;

use serde_json::{Value, json};

use crate::core::{CollectionDefinition, FieldDefinition, collection::GlobalDefinition};

/// Page type identifiers for template conditional logic.
pub enum PageType {
    /// The main administration dashboard.
    Dashboard,
    /// The list of all available collections.
    CollectionList,
    /// The list of items within a specific collection.
    CollectionItems,
    /// The page for editing an existing collection item.
    CollectionEdit,
    /// The page for creating a new collection item.
    CollectionCreate,
    /// The confirmation page for deleting a collection item.
    CollectionDelete,
    /// The list of versions for a specific collection item.
    CollectionVersions,
    /// The page for editing a global's data.
    GlobalEdit,
    /// The list of versions for a specific global.
    GlobalVersions,
    /// The login page.
    AuthLogin,
    /// The forgot password request page.
    AuthForgot,
    /// The password reset page (via email link).
    AuthReset,
    /// The MFA code entry page.
    AuthMfa,
    /// Forbidden error page (403).
    Error403,
    /// Not found error page (404).
    Error404,
    /// Internal server error page (500).
    Error500,
}

impl PageType {
    /// Returns the string identifier used in templates for this page type.
    pub fn as_str(&self) -> &'static str {
        match self {
            PageType::Dashboard => "dashboard",
            PageType::CollectionList => "collection_list",
            PageType::CollectionItems => "collection_items",
            PageType::CollectionEdit => "collection_edit",
            PageType::CollectionCreate => "collection_create",
            PageType::CollectionDelete => "collection_delete",
            PageType::CollectionVersions => "collection_versions",
            PageType::GlobalEdit => "global_edit",
            PageType::GlobalVersions => "global_versions",
            PageType::AuthLogin => "auth_login",
            PageType::AuthForgot => "auth_forgot",
            PageType::AuthReset => "auth_reset",
            PageType::AuthMfa => "auth_mfa",
            PageType::Error403 => "error_403",
            PageType::Error404 => "error_404",
            PageType::Error500 => "error_500",
        }
    }
}

/// A breadcrumb entry with a label and optional URL.
pub struct Breadcrumb {
    /// The text label to display for the breadcrumb.
    pub label: String,
    /// The optional URL to link to. If None, the breadcrumb is the current page.
    pub url: Option<String>,
    /// Optional interpolation param for `{{t label name=label_name}}`.
    pub label_name: Option<String>,
}

impl Breadcrumb {
    /// Create a breadcrumb with a clickable link.
    pub fn link(label: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            url: Some(url.into()),
            label_name: None,
        }
    }
    /// Create a breadcrumb representing the current page (non-clickable).
    pub fn current(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            url: None,
            label_name: None,
        }
    }

    /// Fluent setter for the label interpolation name param.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.label_name = Some(name.into());
        self
    }
}

/// Build the enriched collection context from a definition.
pub fn build_collection_context(def: &CollectionDefinition) -> Value {
    json!({
        "slug": def.slug,
        "display_name": def.display_name(),
        "singular_name": def.singular_name(),
        "title_field": def.title_field(),
        "timestamps": def.timestamps,
        "is_auth": def.is_auth_collection(),
        "is_upload": def.is_upload_collection(),
        "has_drafts": def.has_drafts(),
        "has_versions": def.has_versions(),
        "soft_delete": def.soft_delete,
        "can_permanently_delete": def.access.delete.is_some(),
        "admin": {
            "use_as_title": def.admin.use_as_title,
            "default_sort": def.admin.default_sort,
            "hidden": def.admin.hidden,
            "list_searchable_fields": def.admin.list_searchable_fields,
        },
        "upload": def.upload.as_ref().map(|u| json!({
            "enabled": u.enabled,
            "mime_types": u.mime_types,
            "max_file_size": u.max_file_size,
            "admin_thumbnail": u.admin_thumbnail,
        })),
        "versions": def.versions.as_ref().map(|v| json!({
            "drafts": v.drafts,
            "max_versions": v.max_versions,
        })),
        "auth": def.auth.as_ref().map(|a| json!({
            "enabled": a.enabled,
            "disable_local": a.disable_local,
            "verify_email": a.verify_email,
        })),
        "fields_meta": build_fields_meta(&def.fields),
    })
}

/// Build the enriched global context from a definition.
pub fn build_global_context(def: &GlobalDefinition) -> Value {
    json!({
        "slug": def.slug,
        "display_name": def.display_name(),
        "has_drafts": def.has_drafts(),
        "has_versions": def.has_versions(),
        "versions": def.versions.as_ref().map(|v| json!({
            "drafts": v.drafts,
            "max_versions": v.max_versions,
        })),
        "fields_meta": build_fields_meta(&def.fields),
    })
}

/// Build field metadata array for template conditional logic.
pub fn build_fields_meta(fields: &[FieldDefinition]) -> Value {
    let meta: Vec<Value> = fields
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "field_type": f.field_type.as_str(),
                "required": f.required,
                "unique": f.unique,
                "localized": f.localized,
                "admin": {
                    "label": f.admin.label.as_ref().map(|ls| ls.resolve_default()),
                    "hidden": f.admin.hidden,
                    "readonly": f.admin.readonly,
                    "width": f.admin.width,
                    "description": f.admin.description.as_ref().map(|ls| ls.resolve_default()),
                    "placeholder": f.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                },
            })
        })
        .collect();
    Value::Array(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::{
        collection::{GlobalDefinition, Labels},
        field::{FieldAdmin, FieldType, LocalizedString},
    };

    // --- PageType::as_str ---

    #[test]
    fn page_type_as_str_covers_all_variants() {
        assert_eq!(PageType::Dashboard.as_str(), "dashboard");
        assert_eq!(PageType::CollectionList.as_str(), "collection_list");
        assert_eq!(PageType::CollectionItems.as_str(), "collection_items");
        assert_eq!(PageType::CollectionEdit.as_str(), "collection_edit");
        assert_eq!(PageType::CollectionCreate.as_str(), "collection_create");
        assert_eq!(PageType::CollectionDelete.as_str(), "collection_delete");
        assert_eq!(PageType::CollectionVersions.as_str(), "collection_versions");
        assert_eq!(PageType::GlobalEdit.as_str(), "global_edit");
        assert_eq!(PageType::GlobalVersions.as_str(), "global_versions");
        assert_eq!(PageType::AuthLogin.as_str(), "auth_login");
        assert_eq!(PageType::AuthForgot.as_str(), "auth_forgot");
        assert_eq!(PageType::AuthReset.as_str(), "auth_reset");
        assert_eq!(PageType::Error403.as_str(), "error_403");
        assert_eq!(PageType::Error404.as_str(), "error_404");
        assert_eq!(PageType::Error500.as_str(), "error_500");
    }

    // --- Breadcrumb ---

    #[test]
    fn breadcrumb_link_has_url() {
        let bc = Breadcrumb::link("Home", "/admin");
        assert_eq!(bc.label, "Home");
        assert_eq!(bc.url, Some("/admin".to_string()));
    }

    #[test]
    fn breadcrumb_current_has_no_url() {
        let bc = Breadcrumb::current("Current Page");
        assert_eq!(bc.label, "Current Page");
        assert!(bc.url.is_none());
    }

    // --- build_collection_context ---

    #[test]
    fn build_collection_context_includes_all_fields() {
        let mut def = CollectionDefinition::new("posts");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        };
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
        ];
        let ctx = build_collection_context(&def);
        assert_eq!(ctx["slug"], "posts");
        assert_eq!(ctx["display_name"], "Posts");
        assert_eq!(ctx["singular_name"], "Post");
        assert_eq!(ctx["timestamps"], true);
        assert_eq!(ctx["is_auth"], false);
        assert_eq!(ctx["is_upload"], false);
        assert_eq!(ctx["has_drafts"], false);
        assert_eq!(ctx["has_versions"], false);
        assert_eq!(ctx["soft_delete"], false);
        let meta = ctx["fields_meta"].as_array().unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0]["name"], "title");
    }

    #[test]
    fn build_collection_context_soft_delete_enabled() {
        let mut def = CollectionDefinition::new("pages");
        def.soft_delete = true;
        let ctx = build_collection_context(&def);
        assert_eq!(ctx["soft_delete"], true);
    }

    #[test]
    fn build_collection_context_can_permanently_delete_true() {
        let mut def = CollectionDefinition::new("pages");
        def.access.delete = Some("access.admin_only".to_string());
        let ctx = build_collection_context(&def);
        assert_eq!(ctx["can_permanently_delete"], true);
    }

    #[test]
    fn build_collection_context_can_permanently_delete_false() {
        let def = CollectionDefinition::new("pages");
        let ctx = build_collection_context(&def);
        assert_eq!(ctx["can_permanently_delete"], false);
    }

    // --- build_global_context ---

    #[test]
    fn build_global_context_includes_all_fields() {
        let mut def = GlobalDefinition::new("settings");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Settings".to_string())),
            plural: None,
        };
        def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
        let ctx = build_global_context(&def);
        assert_eq!(ctx["slug"], "settings");
        assert_eq!(ctx["display_name"], "Settings");
        assert_eq!(ctx["has_drafts"], false);
        assert_eq!(ctx["has_versions"], false);
        let meta = ctx["fields_meta"].as_array().unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0]["name"], "site_name");
    }

    // --- build_fields_meta ---

    #[test]
    fn build_fields_meta_includes_admin_info() {
        let field = FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .unique(true)
            .localized(true)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Title".to_string()))
                    .hidden(false)
                    .readonly(true)
                    .width("50%")
                    .description(LocalizedString::Plain("The title field".to_string()))
                    .placeholder(LocalizedString::Plain("Enter title".to_string()))
                    .build(),
            )
            .build();
        let meta = build_fields_meta(&[field]);
        let arr = meta.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let m = &arr[0];
        assert_eq!(m["name"], "title");
        assert_eq!(m["field_type"], "text");
        assert_eq!(m["required"], true);
        assert_eq!(m["unique"], true);
        assert_eq!(m["localized"], true);
        assert_eq!(m["admin"]["label"], "Title");
        assert_eq!(m["admin"]["hidden"], false);
        assert_eq!(m["admin"]["readonly"], true);
        assert_eq!(m["admin"]["width"], "50%");
        assert_eq!(m["admin"]["description"], "The title field");
        assert_eq!(m["admin"]["placeholder"], "Enter title");
    }

    #[test]
    fn build_fields_meta_empty_fields() {
        let meta = build_fields_meta(&[]);
        assert_eq!(meta, Value::Array(vec![]));
    }
}
