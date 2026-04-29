//! Typed page context for filesystem-routed custom admin pages.
//!
//! Drop a template at `<config_dir>/templates/pages/<slug>.hbs` and it
//! auto-routes to `/admin/p/<slug>` against this context. Frontmatter
//! comments (`@nav-section`, `@nav-label`, `@nav-icon`) drive the
//! sidebar entry.

use schemars::JsonSchema;
use serde::Serialize;

use super::BasePageContext;

/// `/admin/p/<slug>` page context.
#[derive(Serialize, JsonSchema)]
pub struct CustomPage {
    #[serde(flatten)]
    pub base: BasePageContext,

    /// Slug from the URL — also the filename stem of the rendered
    /// template (e.g. `status` → `templates/pages/status.hbs`).
    pub slug: String,
}
