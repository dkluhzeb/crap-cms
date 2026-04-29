//! Custom admin page registry — populated from Lua via
//! [`crap.pages.register`].
//!
//! Customer flow:
//!
//! 1. Drop a template at `<config_dir>/templates/pages/<slug>.hbs`. The
//!    Handlebars overlay loader picks it up and the route
//!    `/admin/p/<slug>` renders it.
//! 2. Optionally call `crap.pages.register("<slug>", { ... })` in
//!    `init.lua` to add a sidebar entry. Pages without a registration
//!    still route — they just don't appear in the nav.
//! 3. For dynamic data on the page, use the existing
//!    `crap.template_data.register(name, fn)` + `{{data "name"}}`
//!    helper. No separate "page data" mechanism — same pattern as for
//!    slot widgets.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

/// Sidebar metadata declared from Lua via `crap.pages.register`.
#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
pub struct CustomPage {
    /// Slug — the URL segment and the filename stem.
    pub slug: String,

    /// Sidebar section heading. `None` → page is registered but not
    /// grouped (renders ungrouped at the bottom).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,

    /// Sidebar label. `None` → page is registered but not shown in nav.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Optional Material Symbols icon name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Optional Lua function-ref name for access control. When set, the
    /// named function is called with the page context before the route
    /// handler renders; returning `false` produces a 403, and the page
    /// is hidden from the sidebar nav for users who can't read it.
    /// Mirrors `access.read` on collections / globals — register the
    /// function once via `crap.access.register("name", fn)`, then refer
    /// to it by name here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

/// Registered custom pages, keyed by slug. Populated once during
/// Lua-init from the `_crap_custom_pages` named registry table.
#[derive(Clone, Debug, Default)]
pub struct CustomPageRegistry {
    pages: BTreeMap<String, CustomPage>,
}

impl CustomPageRegistry {
    /// Construct from an iterator of registered pages.
    pub fn from_pages(iter: impl IntoIterator<Item = CustomPage>) -> Self {
        let mut pages = BTreeMap::new();
        for p in iter {
            pages.insert(p.slug.clone(), p);
        }
        Self { pages }
    }

    /// Whether a page is registered under this slug. **Note:** a page
    /// without a registration still routes (template existence is enough);
    /// this only tells you whether `crap.pages.register` was called for
    /// it.
    pub fn is_registered(&self, slug: &str) -> bool {
        self.pages.contains_key(slug)
    }

    /// Look up a registered page by slug.
    pub fn get(&self, slug: &str) -> Option<&CustomPage> {
        self.pages.get(slug)
    }

    /// All pages with a sidebar `label` set, ordered by slug.
    pub fn nav_entries(&self) -> Vec<&CustomPage> {
        self.pages.values().filter(|p| p.label.is_some()).collect()
    }

    /// All registered pages, ordered by slug.
    pub fn all(&self) -> Vec<&CustomPage> {
        self.pages.values().collect()
    }
}

/// Slug validation — restrict to safe characters to avoid path traversal
/// or odd Handlebars template names. Used by both the registration
/// (rejects bad slugs at register-time) and the route handler (rejects
/// bad URLs).
pub fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation_rejects_bad_names() {
        assert!(is_valid_slug("status"));
        assert!(is_valid_slug("system-status"));
        assert!(is_valid_slug("status_2"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("../etc/passwd"));
        assert!(!is_valid_slug("with space"));
        assert!(!is_valid_slug("with.dot"));
    }

    #[test]
    fn registry_filters_nav_entries_to_those_with_label() {
        let reg = CustomPageRegistry::from_pages([
            CustomPage {
                slug: "with_label".into(),
                label: Some("Foo".into()),
                ..Default::default()
            },
            CustomPage {
                slug: "no_label".into(),
                ..Default::default()
            },
        ]);

        let nav = reg.nav_entries();
        assert_eq!(nav.len(), 1);
        assert_eq!(nav[0].slug, "with_label");
    }

    #[test]
    fn registry_lookup_works() {
        let reg = CustomPageRegistry::from_pages([CustomPage {
            slug: "status".into(),
            label: Some("Status".into()),
            ..Default::default()
        }]);

        assert!(reg.is_registered("status"));
        assert!(!reg.is_registered("missing"));
        assert_eq!(reg.get("status").unwrap().label.as_deref(), Some("Status"));
    }
}
