//! Field names: duplicates at one level, reserved and malformed names.

use anyhow::Result;
use mlua::Lua;

use crate::{
    core::{FieldDefinition, FieldType},
    hooks::lua_api::parse::fields::parse_fields,
};

/// BUG-2 regression: two sibling fields with the same name must fail
/// at parse time instead of silently overwriting each other at runtime.
#[test]
fn parse_fields_rejects_duplicate_name_at_same_level() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();

    let f1 = lua.create_table().unwrap();
    f1.set("name", "title").unwrap();
    f1.set("type", "text").unwrap();
    fields_tbl.set(1, f1).unwrap();

    let f2 = lua.create_table().unwrap();
    f2.set("name", "title").unwrap();
    f2.set("type", "text").unwrap();
    fields_tbl.set(2, f2).unwrap();

    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("title") && msg.contains("Duplicate"),
        "expected duplicate-name error, got: {msg}"
    );
}

/// BUG-2 regression: layout wrappers (Row) are transparent, so a name
/// repeated between a top-level field and a Row child counts as a duplicate.
#[test]
fn parse_fields_rejects_duplicate_across_layout_wrappers() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();

    // Top-level: title
    let f1 = lua.create_table().unwrap();
    f1.set("name", "title").unwrap();
    f1.set("type", "text").unwrap();
    fields_tbl.set(1, f1).unwrap();

    // Row wrapping another "title" — also transparent → duplicate.
    let row = lua.create_table().unwrap();
    row.set("name", "row1").unwrap();
    row.set("type", "row").unwrap();
    let row_fields = lua.create_table().unwrap();
    let inner = lua.create_table().unwrap();
    inner.set("name", "title").unwrap();
    inner.set("type", "text").unwrap();
    row_fields.set(1, inner).unwrap();
    row.set("fields", row_fields).unwrap();
    fields_tbl.set(2, row).unwrap();

    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("title"),
        "expected duplicate-name error naming the field, got: {msg}"
    );
}

/// BUG-2 regression: Group fields create their own namespace
/// (columns are prefixed `group__name`), so the same sub-field name
/// can appear in two different groups.
#[test]
fn parse_fields_allows_same_name_in_different_groups() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();

    let mk_group = |slot: i64, name: &str| {
        let g = lua.create_table().unwrap();
        g.set("name", name).unwrap();
        g.set("type", "group").unwrap();
        let sub = lua.create_table().unwrap();
        let s = lua.create_table().unwrap();
        s.set("name", "label").unwrap();
        s.set("type", "text").unwrap();
        sub.set(1, s).unwrap();
        g.set("fields", sub).unwrap();
        fields_tbl.set(slot, g).unwrap();
    };

    mk_group(1, "hero");
    mk_group(2, "footer");

    // Should NOT error — `hero.label` and `footer.label` are distinct columns.
    parse_fields(&lua, &fields_tbl).expect("groups namespace sub-fields independently");
}

/// Helper: parse a single-field collection with the given name/type.
fn parse_one(lua: &Lua, name: &str, ty: &str) -> Result<Vec<FieldDefinition>> {
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", name).unwrap();
    field.set("type", ty).unwrap();
    fields_tbl.set(1, field).unwrap();
    parse_fields(lua, &fields_tbl)
}

/// Regression: a field named after a non-`_` system column (`id`,
/// `parent_id`, `created_at`, `updated_at`) must be rejected at parse
/// time — it would otherwise collide with an auto-generated column.
#[test]
fn parse_rejects_reserved_system_column_names() {
    let lua = Lua::new();
    for name in ["id", "parent_id", "created_at", "updated_at"] {
        let err = parse_one(&lua, name, "text").unwrap_err().to_string();
        assert!(
            err.contains("reserved") && err.contains(name),
            "expected reserved-name rejection for '{name}', got: {err}"
        );
    }
}

/// Regression: a present-but-unknown field `type` must be rejected, not
/// silently coerced to `Text` (which would freeze the wrong column shape
/// and lock out ever adding a real field type of that name).
#[test]
fn parse_rejects_unknown_field_type() {
    let lua = Lua::new();
    for ty in ["slug", "tex", "relation", "Text ", "richtext2"] {
        let err = parse_one(&lua, "f", ty).unwrap_err().to_string();
        assert!(
            err.contains("unknown field type") && err.contains(ty.trim()),
            "expected unknown-type rejection for '{ty}', got: {err}"
        );
    }
}

/// An omitted `type` still defaults to text (absent ≠ unknown).
#[test]
fn parse_defaults_absent_type_to_text() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "f").unwrap();
    fields_tbl.set(1, field).unwrap();
    let parsed = parse_fields(&lua, &fields_tbl).expect("absent type defaults to text");
    assert_eq!(parsed[0].field_type, FieldType::Text);
}

/// Regression: a field whose name starts with `_` must be rejected — the
/// underscore prefix is reserved for system columns.
#[test]
fn parse_rejects_underscore_prefixed_field_names() {
    let lua = Lua::new();
    for name in ["_status", "_ref_count", "_order", "_internal"] {
        let err = parse_one(&lua, name, "text").unwrap_err().to_string();
        assert!(
            err.contains("underscore"),
            "expected underscore rejection for '{name}', got: {err}"
        );
    }
}

/// Regression: `_tz` / `_lang` suffixes are reserved for the timezone /
/// language companion columns — a field named `start_date_tz` would
/// collide with the companion of `start_date`.
#[test]
fn parse_rejects_tz_and_lang_suffix_field_names() {
    let lua = Lua::new();
    for name in ["start_date_tz", "snippet_lang", "event_tz", "content_lang"] {
        let err = parse_one(&lua, name, "text").unwrap_err().to_string();
        assert!(
            err.contains("reserved") && (err.contains("_tz") || err.contains("_lang")),
            "expected suffix rejection for '{name}', got: {err}"
        );
    }
}

/// A field whose name merely *contains* (but doesn't start with) an
/// underscore, or shares a prefix with a reserved name, is still valid.
#[test]
fn parse_allows_non_reserved_names_near_system_columns() {
    let lua = Lua::new();
    for name in ["identifier", "parent", "created", "order", "status"] {
        parse_one(&lua, name, "text")
            .unwrap_or_else(|e| panic!("'{name}' should be a valid field name, got: {e}"));
    }
}
