//! Join targets and flags, and date bounds, are checked at load.

use anyhow::Result;
use mlua::{Lua, Table};

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

/// A Join is virtual/read-only, so `required` / `localized` /
/// `required_locales` are meaningless and rejected at load (a
/// `localized + required` Join previously wedged non-draft writes).
fn join_with_flag(set_flag: impl Fn(&Table)) -> String {
    let lua = Lua::new();
    field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "posts").unwrap();
        f.set("on", "author").unwrap();
        set_flag(f);
    })
    .unwrap_err()
    .to_string()
}

#[test]
fn test_join_required_flag_rejected() {
    let err = join_with_flag(|f| f.set("required", true).unwrap());
    assert!(
        err.contains("required") && err.contains("meaningless"),
        "{err}"
    );
}

#[test]
fn test_join_localized_flag_rejected() {
    let err = join_with_flag(|f| f.set("localized", true).unwrap());
    assert!(
        err.contains("localized") && err.contains("meaningless"),
        "{err}"
    );
}

#[test]
fn test_join_required_locales_rejected() {
    let err = join_with_flag(|f| f.set("required_locales", "all").unwrap());
    assert!(
        err.contains("required_locales") && err.contains("meaningless"),
        "{err}"
    );
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
