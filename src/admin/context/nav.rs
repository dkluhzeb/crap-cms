//! Sidebar navigation context — the lists of collections and globals shown in
//! the left sidebar.
//!
//! Sorted alphabetically by slug; filtered down to entries the current user
//! can read by `BasePageContext::for_handler`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use crate::{admin::AdminState, typegen::LuaAnnotation};

/// Top-level nav data exposed at `{{nav.*}}`.
#[derive(Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.nav")]
pub struct NavData {
    pub collections: Vec<NavCollection>,
    pub globals: Vec<NavGlobal>,
    /// Custom admin pages registered via `crap.pages.register` that the
    /// viewer may open, ordered by slug. Only pages with a `label` appear.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[lua(optional)]
    pub custom_pages: Vec<NavPage>,
    /// The same pages grouped for the sidebar: one section per `section`
    /// heading (alphabetical), then the ungrouped pages last (no heading).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[lua(optional)]
    pub custom_page_sections: Vec<NavPageSection>,
}

/// One custom admin page in the sidebar nav. Carries only what the nav
/// renders — the page's access rule stays server-side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.nav_page")]
pub struct NavPage {
    /// Slug — the URL segment under `/admin/p/`.
    pub slug: String,
    /// Sidebar label.
    pub label: String,
    /// Sidebar section heading; absent for an ungrouped page.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub section: Option<String>,
    /// Material Symbols icon name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub icon: Option<String>,
}

/// A group of custom pages in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.nav_page_section")]
pub struct NavPageSection {
    /// Section heading; absent for the trailing group of ungrouped pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub heading: Option<String>,
    /// The section's pages, ordered by slug.
    pub pages: Vec<NavPage>,
}

/// One sidebar entry for a collection.
#[derive(Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.nav_collection")]
pub struct NavCollection {
    pub slug: String,
    pub display_name: String,
    pub is_auth: bool,
    pub is_upload: bool,
}

/// One sidebar entry for a global.
#[derive(Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.nav_global")]
pub struct NavGlobal {
    pub slug: String,
    pub display_name: String,
}

impl NavData {
    /// Build sidebar nav from the registry. Sorted alphabetically by slug.
    pub fn from_state(state: &AdminState) -> Self {
        let mut collections: Vec<NavCollection> = state
            .infra
            .registry
            .collections
            .values()
            .map(|def| NavCollection {
                slug: def.slug.to_string(),
                display_name: def.display_name().to_string(),
                is_auth: def.is_auth_collection(),
                is_upload: def.is_upload_collection(),
            })
            .collect();
        collections.sort_by(|a, b| a.slug.cmp(&b.slug));

        let mut globals: Vec<NavGlobal> = state
            .infra
            .registry
            .globals
            .values()
            .map(|def| NavGlobal {
                slug: def.slug.to_string(),
                display_name: def.display_name().to_string(),
            })
            .collect();
        globals.sort_by(|a, b| a.slug.cmp(&b.slug));

        let custom_pages: Vec<NavPage> = state
            .custom_pages
            .nav_entries()
            .into_iter()
            .filter_map(|page| {
                Some(NavPage {
                    slug: page.slug.clone(),
                    label: page.label.clone()?,
                    section: page.section.clone(),
                    icon: page.icon.clone(),
                })
            })
            .collect();

        Self {
            collections,
            globals,
            custom_page_sections: sections_of(&custom_pages),
            custom_pages,
        }
    }

    /// Keep only the custom pages `keep` accepts, regrouping the sections so
    /// the flat list and the sidebar sections never disagree.
    pub fn retain_custom_pages(&mut self, mut keep: impl FnMut(&NavPage) -> bool) {
        self.custom_pages.retain(|page| keep(page));
        self.custom_page_sections = sections_of(&self.custom_pages);
    }
}

/// Group pages into sidebar sections: named sections alphabetically by
/// heading, then the ungrouped pages. Pages keep their (slug) order.
fn sections_of(pages: &[NavPage]) -> Vec<NavPageSection> {
    let mut named: BTreeMap<&str, Vec<NavPage>> = BTreeMap::new();
    let mut ungrouped = Vec::new();

    for page in pages {
        match page.section.as_deref() {
            Some(heading) => named.entry(heading).or_default().push(page.clone()),
            None => ungrouped.push(page.clone()),
        }
    }

    let mut sections: Vec<NavPageSection> = named
        .into_iter()
        .map(|(heading, pages)| NavPageSection {
            heading: Some(heading.to_string()),
            pages,
        })
        .collect();

    if !ungrouped.is_empty() {
        sections.push(NavPageSection {
            heading: None,
            pages: ungrouped,
        });
    }

    sections
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::admin::{Translations, templates::create_handlebars};

    fn page(slug: &str, section: Option<&str>) -> NavPage {
        NavPage {
            slug: slug.into(),
            label: slug.to_uppercase(),
            section: section.map(str::to_string),
            icon: None,
        }
    }

    fn headings(sections: &[NavPageSection]) -> Vec<Option<&str>> {
        sections.iter().map(|s| s.heading.as_deref()).collect()
    }

    /// Regression: the documented `section` option was never rendered; pages
    /// now group under their heading with the ungrouped pages last.
    #[test]
    fn pages_group_by_section_with_ungrouped_last() {
        let pages = vec![
            page("a", None),
            page("b", Some("Tools")),
            page("c", Some("Reports")),
            page("d", Some("Tools")),
        ];

        let sections = sections_of(&pages);

        assert_eq!(
            headings(&sections),
            vec![Some("Reports"), Some("Tools"), None]
        );
        let tools: Vec<&str> = sections[1].pages.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(tools, vec!["b", "d"]);
        assert_eq!(sections[2].pages[0].slug, "a");
    }

    #[test]
    fn retaining_pages_regroups_the_sections() {
        let pages = vec![page("a", None), page("b", Some("Tools"))];
        let mut nav = NavData {
            collections: Vec::new(),
            globals: Vec::new(),
            custom_page_sections: sections_of(&pages),
            custom_pages: pages,
        };

        nav.retain_custom_pages(|p| p.slug != "b");

        assert_eq!(headings(&nav.custom_page_sections), vec![None]);
        assert_eq!(nav.custom_pages.len(), 1);
    }

    /// The nav serializes what the sidebar renders — never a page's access
    /// rule, which may carry options.
    #[test]
    fn nav_pages_serialize_without_access() {
        let value = serde_json::to_value(page("a", Some("Tools"))).unwrap();

        assert_eq!(
            value,
            json!({ "slug": "a", "label": "A", "section": "Tools" })
        );
    }

    /// The sidebar renders each section's heading before its pages, and the
    /// ungrouped pages last.
    #[test]
    fn the_sidebar_renders_sections_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let translations = Arc::new(Translations::load(tmp.path()));
        let hbs = create_handlebars(tmp.path(), false, translations, None).unwrap();

        let pages = vec![page("loose", None), page("stats", Some("Reports"))];
        let nav = NavData {
            collections: Vec::new(),
            globals: Vec::new(),
            custom_page_sections: sections_of(&pages),
            custom_pages: pages,
        };

        let html = hbs
            .render("layout/sidebar", &json!({ "nav": nav }))
            .unwrap();

        let heading = html
            .find("sidebar__section-heading")
            .expect("heading rendered");
        let stats = html.find("/admin/p/stats").expect("sectioned page");
        let loose = html.find("/admin/p/loose").expect("ungrouped page");
        assert!(html.contains(">Reports</li>"), "{html}");
        assert!(heading < stats && stats < loose, "{html}");
    }
}
