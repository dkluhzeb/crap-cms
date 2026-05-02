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
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::hooks::lifecycle::InitPhase;

/// Named registry value that holds the `slug → page-table` map.
pub(crate) const PAGES_KEY: &str = "_crap_custom_pages";

/// Register `crap.pages.register` and the storage table.
pub(super) fn register_pages(lua: &Lua, crap: &Table) -> Result<()> {
    lua.set_named_registry_value(PAGES_KEY, lua.create_table()?)?;

    let t = lua.create_table()?;

    t.set(
        "register",
        lua.create_function(|lua, (slug, opts): (String, Table)| register_page(lua, &slug, opts))?,
    )?;
    t.set("list", lua.create_function(|lua, ()| list_pages(lua))?)?;

    crap.set("pages", t)?;

    Ok(())
}

fn register_page(lua: &Lua, slug: &str, opts: Table) -> LuaResult<()> {
    // Custom pages are read into `AdminState.custom_pages` once at startup
    // (see `admin::server::start`). A runtime call would only land in the
    // current VM's named registry and never reach the live registry, so the
    // sidebar entry would silently fail to appear and the route would not
    // be added. Refuse explicitly with a pointer to the right place.
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(
            "crap.pages.register must be called from init.lua or a definition file \
             — runtime registration has no effect on the sidebar or routes"
                .into(),
        ));
    }

    if !is_valid_slug(slug) {
        return Err(RuntimeError(format!(
            "crap.pages.register: invalid slug {slug:?} (use a-z, 0-9, '-', '_')"
        )));
    }

    let pages: Table = lua.named_registry_value(PAGES_KEY)?;
    pages.set(slug, opts)?;
    Ok(())
}

fn list_pages(lua: &Lua) -> LuaResult<Table> {
    let pages: Table = lua.named_registry_value(PAGES_KEY)?;
    let names = lua.create_table()?;
    let mut i = 1;
    for pair in pages.pairs::<Value, Value>() {
        let (key, _) = pair?;
        names.set(i, key)?;
        i += 1;
    }
    Ok(names)
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
        let crap = lua.create_table().unwrap();
        register_pages(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();
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
        let crap = lua.create_table().unwrap();
        register_pages(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();
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
