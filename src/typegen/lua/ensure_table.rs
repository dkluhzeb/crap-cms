//! Walk-or-create helper for `lua_table!` registrations.
//!
//! Each `lua_table!` invocation declares a Lua path like `"crap.collections.config"`.
//! At registration time we need to look up (or create) every intermediate
//! table along that path so the leaf table can be populated. `ensure_table`
//! does this in one call, idempotently — sibling registrations into the
//! same parent table all share the parent.
//!
//! Lookup starts from `lua.globals()` and walks the path segment-by-segment.
//! Missing tables are created. Non-table values at any path position are
//! a hard error (means a registration would overwrite something).

use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

/// Walk `path` from the Lua globals, creating intermediate tables as
/// needed, and return the leaf table.
///
/// `path` is the dotted Lua path split into segments — e.g.
/// `&["crap", "collections", "config"]` for `crap.collections.config`.
///
/// # Errors
///
/// Returns an error if any path position holds a non-table value (would
/// be overwritten on registration — almost certainly a bug).
pub fn ensure_table(lua: &Lua, path: &[&str]) -> LuaResult<Table> {
    if path.is_empty() {
        return Err(RuntimeError(
            "ensure_table: path must have at least one segment".into(),
        ));
    }

    let mut current = ensure_global(lua, path[0])?;

    for seg in &path[1..] {
        current = ensure_child(lua, &current, seg, path)?;
    }

    Ok(current)
}

/// Get-or-create a global table by name.
fn ensure_global(lua: &Lua, name: &str) -> LuaResult<Table> {
    let globals = lua.globals();
    match globals.get::<Value>(name)? {
        Value::Table(t) => Ok(t),
        Value::Nil => {
            let t = lua.create_table()?;
            globals.set(name, t.clone())?;
            Ok(t)
        }
        other => Err(RuntimeError(format!(
            "ensure_table: global `{name}` is not a table (got {ty}); cannot register Lua API here",
            ty = other.type_name()
        ))),
    }
}

/// Get-or-create a child of `parent` named `child_name`. `full_path` is
/// passed for error context.
fn ensure_child(
    lua: &Lua,
    parent: &Table,
    child_name: &str,
    full_path: &[&str],
) -> LuaResult<Table> {
    match parent.get::<Value>(child_name)? {
        Value::Table(t) => Ok(t),
        Value::Nil => {
            let t = lua.create_table()?;
            parent.set(child_name, t.clone())?;
            Ok(t)
        }
        other => Err(RuntimeError(format!(
            "ensure_table: `{path}` is not a table (got {ty}); cannot register Lua API here",
            path = full_path.join("."),
            ty = other.type_name(),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_single_segment() {
        let lua = Lua::new();
        let t = ensure_table(&lua, &["crap"]).unwrap();
        t.set("marker", true).unwrap();
        let read_back: bool = lua.load("return crap.marker").eval().unwrap();
        assert!(read_back);
    }

    #[test]
    fn creates_nested_chain() {
        let lua = Lua::new();
        let leaf = ensure_table(&lua, &["crap", "collections", "config"]).unwrap();
        leaf.set("fn_here", true).unwrap();
        let exists: bool = lua
            .load("return crap.collections.config.fn_here")
            .eval()
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn reuses_existing_intermediate() {
        // Two registrations into the same parent — `crap.collections.config.get`
        // and `crap.collections.config.list` — must see the same intermediate
        // tables on the second call.
        let lua = Lua::new();
        let first = ensure_table(&lua, &["crap", "collections", "config"]).unwrap();
        first.set("a", 1).unwrap();
        let second = ensure_table(&lua, &["crap", "collections", "config"]).unwrap();
        second.set("b", 2).unwrap();
        let a: i64 = lua.load("return crap.collections.config.a").eval().unwrap();
        let b: i64 = lua.load("return crap.collections.config.b").eval().unwrap();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
    }

    #[test]
    fn errors_when_path_position_is_not_a_table() {
        let lua = Lua::new();
        lua.globals().set("crap", 42).unwrap();
        let err =
            ensure_table(&lua, &["crap", "x"]).expect_err("should refuse to overwrite a non-table");
        assert!(err.to_string().contains("not a table"), "got: {err}");
    }

    #[test]
    fn empty_path_errors() {
        let lua = Lua::new();
        assert!(ensure_table(&lua, &[]).is_err());
    }
}
