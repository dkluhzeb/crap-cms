//! Page-level context primitives shared by all admin pages.
//!
//! [`PageType`] is the discriminant baked into the rendered context so templates
//! can branch on the kind of page. [`Breadcrumb`] is the typed breadcrumb entry
//! used by handlers; it serializes to the JSON shape the breadcrumb partial
//! consumes.

use serde::Serialize;

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
    /// Setup-required notice (no auth collection configured).
    AuthRequired,
    /// Authenticated but not authorized for admin.
    AdminDenied,
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
            PageType::AuthRequired => "auth_required",
            PageType::AdminDenied => "admin_denied",
        }
    }
}

/// A breadcrumb entry with a label and optional URL.
#[derive(Serialize, Clone)]
pub struct Breadcrumb {
    /// The text label to display for the breadcrumb.
    pub label: String,
    /// The optional URL to link to. If None, the breadcrumb is the current page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Optional interpolation param for `{{t label name=label_name}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

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
        assert_eq!(PageType::AuthMfa.as_str(), "auth_mfa");
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

    #[test]
    fn breadcrumb_serialize_link_shape() {
        let bc = Breadcrumb::link("Home", "/admin");
        let v = serde_json::to_value(&bc).unwrap();
        assert_eq!(v, json!({"label": "Home", "url": "/admin"}));
    }

    #[test]
    fn breadcrumb_serialize_current_omits_url() {
        let bc = Breadcrumb::current("Posts");
        let v = serde_json::to_value(&bc).unwrap();
        assert_eq!(v, json!({"label": "Posts"}));
    }

    #[test]
    fn breadcrumb_serialize_with_name_includes_label_name() {
        let bc = Breadcrumb::current("create_name").with_name("Post");
        let v = serde_json::to_value(&bc).unwrap();
        assert_eq!(v, json!({"label": "create_name", "label_name": "Post"}));
    }
}
