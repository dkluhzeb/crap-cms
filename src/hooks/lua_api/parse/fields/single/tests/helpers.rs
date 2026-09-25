//! Fixtures shared by the per-field parser tests.

use anyhow::Result;
use mlua::{Lua, Table};

use crate::hooks::lua_api::parse::fields::parse_fields;

/// Parse a one-field list whose field is named `f` and set up by `set`.
pub(super) fn field_with(lua: &Lua, set: impl FnOnce(&Table)) -> Result<()> {
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "f").unwrap();
    set(&field);
    fields_tbl.set(1, field).unwrap();
    parse_fields(lua, &fields_tbl).map(|_| ())
}
