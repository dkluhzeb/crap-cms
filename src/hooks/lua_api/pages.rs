//! Register `crap.pages` — declare custom admin pages and their sidebar
//! metadata from Lua. The page TEMPLATE lives at
//! `<config_dir>/templates/pages/<slug>.hbs` (rendered by Handlebars);
//! this API only adds the sidebar entry and the optional access gate.
//!
//! ## Usage
//!
//! ```lua
//! crap.pages.register("status", {
//!   section = "Tools",                  -- optional sidebar section heading
//!   label   = "System status",          -- optional; defaults to title-cased slug
//!   icon    = "heart-pulse",            -- optional Material Symbols icon
//!   access  = "access.admin_only",      -- optional Lua function ref
//! })
//! ```
//!
//! Recognized keys: `section`, `label`, `icon`, `access`. All four are
//! optional — every key may be omitted, in which case the page still
//! routes at `/admin/p/<slug>` but no sidebar entry is rendered (when
//! `label` is missing) and no access gate runs (when `access` is missing).
//! `access` is a Lua function reference name (e.g. `"access.admin_only"`)
//! resolved against the same registry the collection-level `access.*`
//! entries use; **not** a role string.
//!
//! For dynamic page data, use the existing `crap.template_data.register`
//! plus the `{{data "name"}}` helper — same pattern as slot widgets, no
//! separate "page data" concept.

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::core::HookRef;
use crate::hooks::lifecycle::InitPhase;
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Sidebar / access metadata for a custom admin page.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.PageOptions")]
pub(crate) struct PageOptions {
    /// Sidebar section heading (e.g., `"Tools"`).
    pub(crate) section: Option<String>,
    /// Sidebar label (defaults to title-cased slug when omitted).
    pub(crate) label: Option<String>,
    /// Material Symbols icon name.
    pub(crate) icon: Option<String>,
    /// Lua function ref for access control (resolved against the same
    /// registry as collection-level `access.*`). A bare ref string or a
    /// `{ ref, options }` table whose options reach the gate as `ctx.options`.
    #[lua(ty = "string | crap.HookRef")]
    pub(crate) access: Option<HookRef>,
}

impl FromLua for PageOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Named registry value that holds the `slug → page-table` map.
pub(crate) const PAGES_KEY: &str = "_crap_custom_pages";

/// Register a custom admin page. Must be called from init.lua or a
/// definition file — runtime registration is rejected (custom pages are
/// read once at startup, so a runtime call would silently land in the
/// current VM's named registry without ever appearing on the sidebar
/// or routes).
#[lua_fn(path = "crap.pages.register")]
fn page_register(
    lua: &Lua,
    #[lua(doc = "Page slug (a-z, 0-9, '-', '_'); also the route under `/admin/p/<slug>`.")]
    slug: String,
    #[lua(
        ty = "crap.PageOptions",
        doc = "Sidebar / access metadata (all keys optional)."
    )]
    opts: PageOptions,
) -> LuaResult<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(
            "crap.pages.register must be called from init.lua or a definition file \
             — runtime registration has no effect on the sidebar or routes"
                .into(),
        ));
    }

    if !is_valid_slug(&slug) {
        return Err(RuntimeError(format!(
            "crap.pages.register: invalid slug {slug:?} (use a-z, 0-9, '-', '_')"
        )));
    }

    let pages: Table = lua.named_registry_value(PAGES_KEY)?;
    let entry = lua.create_table()?;
    if let Some(s) = &opts.section {
        entry.set("section", s.as_str())?;
    }
    if let Some(l) = &opts.label {
        entry.set("label", l.as_str())?;
    }
    if let Some(i) = &opts.icon {
        entry.set("icon", i.as_str())?;
    }
    if let Some(a) = &opts.access {
        // Store as a bare string or a `{ ref, options }` table so the
        // page-registry round-trip preserves any per-config options.
        match a.options() {
            None => entry.set("access", a.reference())?,
            Some(options) => {
                let t = lua.create_table()?;
                t.set("ref", a.reference())?;
                t.set("options", super::json_to_lua(lua, options)?)?;
                entry.set("access", t)?;
            }
        }
    }
    pages.set(slug, entry)?;
    Ok(())
}

/// List the slugs of every registered custom page (in iteration order
/// — not deterministic across runs).
#[lua_fn(path = "crap.pages.list", returns = "string[]")]
fn page_list(lua: &Lua) -> LuaResult<Table> {
    let pages: Table = lua.named_registry_value(PAGES_KEY)?;
    let names = lua.create_table()?;
    for (i, pair) in (1..).zip(pages.pairs::<Value, Value>()) {
        let (key, _) = pair?;
        names.set(i, key)?;
    }
    Ok(names)
}

lua_table! {
    name: crap_pages,
    path: "crap.pages",
    state: (),
    header: "Declare custom admin pages and their sidebar metadata. The page\nTEMPLATE lives at `<config_dir>/templates/pages/<slug>.hbs`; this\nAPI only adds the sidebar entry and the optional access gate.",
    fns: [page_register, page_list],
}

/// Register `crap.pages.register` and the storage table. Parent `crap`
/// table must already be in globals.
pub(super) fn register_pages(lua: &Lua) -> Result<()> {
    lua.set_named_registry_value(PAGES_KEY, lua.create_table()?)?;
    register_crap_pages(lua, ())?;
    Ok(())
}

/// Mirrors `admin::custom_pages::is_valid_slug` so we can validate
/// slugs without crossing the admin/hooks module boundary.
fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Lua VM with `crap.pages` registered AND the `InitPhase`
    /// marker set, mimicking the state during `execute_init_lua`.
    fn lua_in_init_phase() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_pages(&lua).unwrap();
        lua.set_app_data(InitPhase);
        lua
    }

    #[test]
    fn register_and_lookup_a_page() {
        let lua = lua_in_init_phase();

        lua.load(
            r#"
            crap.pages.register("status", {
              section = "Tools",
              label = "Status",
              icon = "heart-pulse",
            })
        "#,
        )
        .exec()
        .unwrap();

        let pages: Table = lua.named_registry_value(PAGES_KEY).unwrap();
        let entry: Table = pages.get("status").unwrap();
        assert_eq!(entry.get::<String>("section").unwrap(), "Tools");
        assert_eq!(entry.get::<String>("label").unwrap(), "Status");
    }

    #[test]
    fn invalid_slug_is_rejected() {
        let lua = lua_in_init_phase();

        let result = lua.load(r#"crap.pages.register("../bad", {})"#).exec();
        assert!(result.is_err());
    }

    /// Regression: `crap.pages.register` called outside the init phase must
    /// fail loudly. Custom pages are read once at startup; a runtime call
    /// would silently land in the current VM's named registry only and
    /// never reach the live `CustomPageRegistry`.
    #[test]
    fn register_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_pages(&lua).unwrap();
        // Note: NO `set_app_data(InitPhase)` — we're simulating a runtime hook.

        let err = lua
            .load(r#"crap.pages.register("status", {})"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("init.lua") || err.contains("runtime registration"),
            "expected init-only error message, got: {err}"
        );

        let pages: Table = lua.named_registry_value(PAGES_KEY).unwrap();
        let entry: Result<Table, _> = pages.get("status");
        assert!(entry.is_err(), "page must NOT be registered when refused");
    }

    #[test]
    fn list_returns_registered_slugs() {
        let lua = lua_in_init_phase();

        lua.load(
            r#"
            crap.pages.register("status", { label = "S" })
            crap.pages.register("reports", { label = "R" })
        "#,
        )
        .exec()
        .unwrap();

        let names: Table = lua.load("return crap.pages.list()").eval().unwrap();
        assert_eq!(names.raw_len(), 2);
    }
}
