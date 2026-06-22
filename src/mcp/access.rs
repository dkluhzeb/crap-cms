//! `access.mcp` enforcement for the MCP surface.
//!
//! The MCP surface runs with `override_access`, so the service-layer access
//! model never gates it. The per-collection / per-global `access.mcp` key is
//! therefore enforced HERE, at the MCP boundary: a hidden collection is dropped
//! from tool generation, schema introspection, and resources, and its CRUD
//! tools are rejected at execution.
//!
//! `access.mcp` is evaluated **user-independently** — the MCP surface
//! authenticates with a shared key, not a per-user identity — so the result is
//! static per collection. Unset → exposed (permissive default); only
//! collections that actually set `access.mcp` are ever evaluated, so the common
//! case costs nothing.

use std::collections::HashSet;

use tracing::warn;

use crate::{
    core::{HookRef, Registry},
    db::{AccessResult, DbConnection},
    hooks::{AccessCheckInput, HookRunner},
};

/// Whether a single collection/global is exposed to the MCP surface, given its
/// `access.mcp` rule. Unset → exposed (permissive default). When set, evaluated
/// user-independently: `Allowed` → exposed; `Denied`, a filter table (config
/// error — MCP access is boolean), or a hook error → hidden (fail-closed).
pub(in crate::mcp) fn slug_exposed(
    access_mcp: Option<&HookRef>,
    runner: &HookRunner,
    conn: &dyn DbConnection,
    slug: &str,
) -> bool {
    let Some(hook) = access_mcp else {
        return true;
    };

    let result = runner.check_access(
        &AccessCheckInput {
            document: None,
            access: Some(hook),
            user: None,
            id: None,
            data: None,
            locale: None,
            operation: "mcp",
            collection: slug,
            ui_locale: None,
        },
        conn,
    );

    if matches!(result, Ok(AccessResult::Allowed)) {
        return true;
    }

    warn!(
        "'{slug}' is hidden from the MCP surface by access.mcp \
         (denied, errored, or returned a filter table — MCP access is boolean)"
    );
    false
}

/// The set of collection/global slugs HIDDEN from the MCP surface by their
/// `access.mcp` rule. Empty by default — everything exposed.
#[derive(Default)]
pub(in crate::mcp) struct McpExposure {
    hidden: HashSet<String>,
}

impl McpExposure {
    /// Evaluate `access.mcp` for every collection and global that sets it,
    /// collecting the slugs to hide. Collections without `access.mcp` are never
    /// evaluated (no Lua call). A rule that denies, returns a filter table (a
    /// configuration error — MCP access is boolean), or errors hides the slug
    /// (fail-closed).
    pub(in crate::mcp) fn resolve(
        registry: &Registry,
        runner: &HookRunner,
        conn: &dyn DbConnection,
    ) -> Self {
        let collections = registry
            .collections
            .iter()
            .map(|(slug, def)| (slug, def.access.mcp.as_ref()));
        let globals = registry
            .globals
            .iter()
            .map(|(slug, def)| (slug, def.access.mcp.as_ref()));

        let mut hidden = HashSet::new();

        for (slug, mcp_ref) in collections.chain(globals) {
            let slug: &str = slug.as_ref();
            if mcp_ref.is_some() && !slug_exposed(mcp_ref, runner, conn, slug) {
                hidden.insert(slug.to_string());
            }
        }

        Self { hidden }
    }

    /// Whether `slug` is exposed to the MCP surface.
    pub(in crate::mcp) fn allows(&self, slug: &str) -> bool {
        !self.hidden.contains(slug)
    }

    /// Test-only constructor for a fixed hidden set (avoids needing a live VM).
    #[cfg(test)]
    pub(in crate::mcp) fn with_hidden(slugs: &[&str]) -> Self {
        Self {
            hidden: slugs.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_exposes_everything() {
        let exposure = McpExposure::default();
        assert!(exposure.allows("posts"));
        assert!(exposure.allows("anything"));
    }

    #[test]
    fn hidden_slugs_are_not_exposed() {
        let exposure = McpExposure::with_hidden(&["secrets", "internal"]);
        assert!(!exposure.allows("secrets"));
        assert!(!exposure.allows("internal"));
        assert!(exposure.allows("posts"));
    }
}
