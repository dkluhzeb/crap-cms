//! [`PageMeta`] — the `page` object templates branch on for layout decisions
//! (active sidebar item, breadcrumb rendering, body class).

use schemars::JsonSchema;
use serde::Serialize;

use super::{Breadcrumb, PageType};

/// The `page` object every admin template receives. Carries the page-type
/// discriminant, the page title (already-translated label or translation key),
/// optional title interpolation parameter, and breadcrumb trail.
#[derive(Serialize, JsonSchema)]
pub struct PageMeta {
    /// Page-type discriminant. Serialized as a snake_case string literal so
    /// templates can branch with `{{#if (eq page.type "collection_edit")}}`.
    #[serde(rename = "type")]
    pub page_type: &'static str,

    /// Page title or translation key.
    pub title: String,

    /// Optional interpolation param for `{{t page.title name=page.title_name}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_name: Option<String>,

    /// Breadcrumb trail rendered by `partials/breadcrumb.hbs`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub breadcrumbs: Vec<Breadcrumb>,
}

impl PageMeta {
    /// Construct page metadata for the given page type and title. Breadcrumbs
    /// and title_name default to empty/None — callers add them as needed.
    pub fn new(page_type: PageType, title: impl Into<String>) -> Self {
        Self {
            page_type: page_type.as_str(),
            title: title.into(),
            title_name: None,
            breadcrumbs: Vec::new(),
        }
    }

    /// Fluent setter for the title interpolation name parameter.
    pub fn with_title_name(mut self, name: impl Into<String>) -> Self {
        self.title_name = Some(name.into());
        self
    }

    /// Fluent setter for the breadcrumb trail.
    pub fn with_breadcrumbs(mut self, breadcrumbs: Vec<Breadcrumb>) -> Self {
        self.breadcrumbs = breadcrumbs;
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn page_meta_minimal_serializes_without_optional_keys() {
        let meta = PageMeta::new(PageType::Dashboard, "dashboard");
        let v = serde_json::to_value(&meta).unwrap();
        assert_eq!(v, json!({"type": "dashboard", "title": "dashboard"}));
    }

    #[test]
    fn page_meta_with_title_name_includes_it() {
        let meta = PageMeta::new(PageType::CollectionEdit, "edit_name").with_title_name("Post");
        let v = serde_json::to_value(&meta).unwrap();
        assert_eq!(
            v,
            json!({"type": "collection_edit", "title": "edit_name", "title_name": "Post"})
        );
    }

    #[test]
    fn page_meta_with_breadcrumbs_includes_them() {
        let meta = PageMeta::new(PageType::CollectionItems, "posts").with_breadcrumbs(vec![
            Breadcrumb::link("Home", "/admin"),
            Breadcrumb::current("Posts"),
        ]);
        let v = serde_json::to_value(&meta).unwrap();
        assert_eq!(
            v,
            json!({
                "type": "collection_items",
                "title": "posts",
                "breadcrumbs": [
                    {"label": "Home", "url": "/admin"},
                    {"label": "Posts"}
                ]
            })
        );
    }
}
