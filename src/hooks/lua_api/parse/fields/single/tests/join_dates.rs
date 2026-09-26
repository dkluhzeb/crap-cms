//! Join targets and flags, and date bounds, are checked at load.

use anyhow::Result;
use mlua::{IntoLua, Lua, Table, Value};

use super::helpers::field_with;
use crate::{core::FieldDefinition, hooks::lua_api::parse::fields::parse_fields};

// ── join strictness ─────────────────────────────────────────────────

/// Regression: a join field with missing / wrong-typed / empty
/// `collection`/`on` used to silently become empty strings ("validated
/// later" — which never happened) and matched nothing at populate time.
/// All three are now load-time errors.
#[test]
fn test_join_missing_collection_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("on", "author").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("collection"), "{err}");
}

#[test]
fn test_join_non_string_on_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "posts").unwrap();
        f.set("on", true).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("'on'") && err.contains("boolean"), "{err}");
}

#[test]
fn test_join_empty_collection_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "").unwrap();
        f.set("on", "author").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("non-empty"), "{err}");
}

#[test]
fn test_join_valid_config_parses() {
    let lua = Lua::new();
    field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "posts").unwrap();
        f.set("on", "author").unwrap();
    })
    .expect("valid join must parse");
}

/// A join's `limit` — the most documents it lists per document — parses as
/// given and must be an integer of at least 1.
#[test]
fn test_join_limit_parses_and_must_be_positive() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "f").unwrap();
    field.set("type", "join").unwrap();
    field.set("collection", "posts").unwrap();
    field.set("on", "author").unwrap();
    field.set("limit", 5).unwrap();
    fields_tbl.set(1, field).unwrap();

    let parsed = parse_fields(&lua, &fields_tbl).expect("a positive limit parses");
    assert_eq!(parsed[0].join.as_ref().and_then(|j| j.limit), Some(5));

    for bad in [0, -3] {
        let err = join_with_flag(|f| f.set("limit", bad).unwrap());
        assert!(err.contains("limit") && err.contains("at least 1"), "{err}");
    }

    let err = join_with_flag(|f| f.set("limit", "ten").unwrap());
    assert!(err.contains("limit") && err.contains("integer"), "{err}");
}

/// A `fields`-holding container table named `name` of `ty` wrapping `inner`.
fn container(lua: &Lua, name: &str, ty: &str, inner: Table) -> Table {
    let tbl = lua.create_table().unwrap();
    tbl.set("name", name).unwrap();
    tbl.set("type", ty).unwrap();

    let fields = lua.create_table().unwrap();
    fields.set(1, inner).unwrap();
    tbl.set("fields", fields).unwrap();

    tbl
}

fn join_table(lua: &Lua) -> Table {
    let join = lua.create_table().unwrap();
    join.set("name", "related").unwrap();
    join.set("type", "join").unwrap();
    join.set("collection", "posts").unwrap();
    join.set("on", "author").unwrap();
    join
}

fn parse_one_container(lua: &Lua, field: Table) -> Result<Vec<FieldDefinition>> {
    let fields_tbl = lua.create_table().unwrap();
    fields_tbl.set(1, field).unwrap();
    parse_fields(lua, &fields_tbl)
}

/// Regression: a join inside an array or blocks row was accepted, yet a join
/// lists the documents referencing the whole document — every row repeated
/// the same list, and the admin rendered it empty. Refused at any depth
/// under a row; a join in a group stays valid.
#[test]
fn test_join_inside_rows_rejected_but_allowed_in_a_group() {
    let lua = Lua::new();

    let in_array = container(&lua, "rows", "array", join_table(&lua));
    let err = format!("{:#}", parse_one_container(&lua, in_array).unwrap_err());
    assert!(err.contains("join") && err.contains("rows"), "{err}");

    let group = container(&lua, "meta", "group", join_table(&lua));
    let deep = container(&lua, "rows", "array", group);
    let err = format!("{:#}", parse_one_container(&lua, deep).unwrap_err());
    assert!(err.contains("join"), "a join under a group in a row: {err}");

    let block = lua.create_table().unwrap();
    block.set("type", "hero").unwrap();
    let block_fields = lua.create_table().unwrap();
    block_fields.set(1, join_table(&lua)).unwrap();
    block.set("fields", block_fields).unwrap();
    let blocks_list = lua.create_table().unwrap();
    blocks_list.set(1, block).unwrap();
    let blocks = lua.create_table().unwrap();
    blocks.set("name", "content").unwrap();
    blocks.set("type", "blocks").unwrap();
    blocks.set("blocks", blocks_list).unwrap();
    let err = format!("{:#}", parse_one_container(&lua, blocks).unwrap_err());
    assert!(err.contains("join"), "a join in a block: {err}");

    let in_group = container(&lua, "meta", "group", join_table(&lua));
    parse_one_container(&lua, in_group).expect("a join in a group is valid");
}

/// A join table with `set_flag` applied, parsed in `lua`; the error text.
fn join_error_in(lua: &Lua, set_flag: impl FnOnce(&Table)) -> String {
    field_with(lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "posts").unwrap();
        f.set("on", "author").unwrap();
        set_flag(f);
    })
    .unwrap_err()
    .to_string()
}

/// [`join_error_in`] on a fresh Lua state.
fn join_with_flag(set_flag: impl FnOnce(&Table)) -> String {
    join_error_in(&Lua::new(), set_flag)
}

/// Regression: a join accepted every common key, yet only the read-side ones
/// can act on a virtual, never-written field — `unique`, `index`, `validate`,
/// `default_value`, `required_when`, `mcp` were silently inert, and `required`
/// / `localized` were refused only when truthy. Each is refused by presence,
/// its falsy form included.
#[test]
fn inert_keys_on_a_join_are_refused_by_presence() {
    let lua = Lua::new();
    let mcp = lua.create_table().unwrap();
    mcp.set("description", "related posts").unwrap();

    let cases: Vec<(&str, Value)> = vec![
        ("required", Value::Boolean(false)),
        ("localized", Value::Boolean(false)),
        ("unique", Value::Boolean(true)),
        ("index", Value::Boolean(false)),
        ("required_locales", "all".into_lua(&lua).unwrap()),
        ("validate", "hooks.validate".into_lua(&lua).unwrap()),
        ("required_when", "hooks.when".into_lua(&lua).unwrap()),
        ("default_value", "x".into_lua(&lua).unwrap()),
        ("mcp", Value::Table(mcp)),
    ];

    for (key, value) in cases {
        let err = join_error_in(&lua, |f| f.set(key, value).unwrap());

        assert!(
            err.contains(&format!("'{key}'")) && err.contains("no effect on a join"),
            "{key}: {err}"
        );
    }
}

/// Regression: a join accepted `access.create` / `access.update` and every
/// write-side lifecycle hook, none of which can ever run on a field no write
/// carries. Refused by presence; the read side stays accepted.
#[test]
fn write_side_access_and_hooks_on_a_join_are_refused() {
    let lua = Lua::new();

    for (sub, key) in [
        ("access", "create"),
        ("access", "update"),
        ("hooks", "before_validate"),
        ("hooks", "before_change"),
        ("hooks", "after_change"),
    ] {
        let tbl = lua.create_table().unwrap();
        let value: Value = if sub == "hooks" {
            Value::Table(lua.create_sequence_from(["hooks.h"]).unwrap())
        } else {
            "hooks.h".into_lua(&lua).unwrap()
        };
        tbl.set(key, value).unwrap();

        let err = join_error_in(&lua, |f| f.set(sub, tbl).unwrap());

        assert!(
            err.contains(&format!("'{sub}.{key}'")) && err.contains("no effect on a join"),
            "{sub}.{key}: {err}"
        );
    }
}

/// Every key a join does honor parses: its target, `limit`, `hidden`,
/// `admin`, `access.read` and an `after_read` hook.
#[test]
fn a_join_with_every_accepted_key_parses() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let join = join_table(&lua);

    join.set("limit", 5).unwrap();
    join.set("hidden", false).unwrap();

    let admin = lua.create_table().unwrap();
    admin.set("description", "Posts by this author").unwrap();
    join.set("admin", admin).unwrap();

    let access = lua.create_table().unwrap();
    access.set("read", "hooks.access.read").unwrap();
    join.set("access", access).unwrap();

    let hooks = lua.create_table().unwrap();
    hooks
        .set(
            "after_read",
            lua.create_sequence_from(["hooks.shape"]).unwrap(),
        )
        .unwrap();
    join.set("hooks", hooks).unwrap();

    fields_tbl.set(1, join).unwrap();
    let parsed = parse_fields(&lua, &fields_tbl).expect("every accepted join key parses");

    assert!(parsed[0].access.read.is_some());
    assert_eq!(parsed[0].hooks.after_read.len(), 1);
}

// ── date-bound strictness ───────────────────────────────────────────

/// Regression: `min_date`/`max_date` used to silently drop non-string
/// values and accept arbitrary strings — the runtime compares the bound
/// lexically against the value's date part, so a malformed bound never
/// (or always) matched. Wrong type, bad format, and inverted ranges are
/// now load-time errors.
#[test]
fn test_date_bound_non_string_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("min_date", 2024i64).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("min_date"), "{err}");
}

#[test]
fn test_date_bound_bad_format_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("max_date", "01.02.2024").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("YYYY-MM-DD"), "{err}");
}

#[test]
fn test_date_bounds_inverted_range_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("min_date", "2024-12-31").unwrap();
        f.set("max_date", "2024-01-01").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("after max_date"), "{err}");
}

#[test]
fn test_date_bounds_valid_range_parses() {
    let lua = Lua::new();
    field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("min_date", "2024-01-01").unwrap();
        f.set("max_date", "2024-12-31").unwrap();
    })
    .expect("valid date bounds must parse");
}
