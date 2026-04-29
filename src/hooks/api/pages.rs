//! Register `crap.pages` — declare custom admin pages and their sidebar
//! metadata from Lua. The page TEMPLATE lives at
//! `<config_dir>/templates/pages/<slug>.hbs` (rendered by Handlebars);
//! this API only adds the sidebar entry.
//!
//! ## Usage
//!
//! ```lua
//! crap.pages.register("status", {
//!   section = "Tools",
//!   label   = "System status",
//!   icon    = "heart-pulse",
//!   permission = "admin",
//! })
//! ```
//!
//! For dynamic page data, use the existing `crap.template_data.register`
//! plus the `{{data "name"}}` helper — same pattern as slot widgets, no
//! separate "page data" concept.

use anyhow::Result;
use mlua::{Lua, Table, Value};

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

fn register_page(lua: &Lua, slug: &str, opts: Table) -> mlua::Result<()> {
    if !is_valid_slug(slug) {
        return Err(mlua::Error::RuntimeError(format!(
            "crap.pages.register: invalid slug {slug:?} (use a-z, 0-9, '-', '_')"
        )));
    }

    let pages: Table = lua.named_registry_value(PAGES_KEY)?;
    pages.set(slug, opts)?;
    Ok(())
}

fn list_pages(lua: &Lua) -> mlua::Result<Table> {
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

    #[test]
    fn register_and_lookup_a_page() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_pages(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

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
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_pages(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

        let result = lua.load(r#"crap.pages.register("../bad", {})"#).exec();
        assert!(result.is_err());
    }

    #[test]
    fn list_returns_registered_slugs() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_pages(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

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
