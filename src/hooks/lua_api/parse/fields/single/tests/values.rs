//! Parsed values: defaults, containers, date config, bounds, join, mcp and hooks.

use mlua::Lua;
use serde_json::Value as JsonValue;

use crate::{
    core::{HookRef, PickerAppearance},
    hooks::lua_api::parse::fields::parse_fields,
};

#[test]
fn test_parse_field_index() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "status").unwrap();
    field.set("type", "text").unwrap();
    field.set("index", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(fields[0].index, "index should be true");
}

#[test]
fn test_parse_field_index_default_false() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "title").unwrap();
    field.set("type", "text").unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(!fields[0].index, "index should default to false");
}

#[test]
fn test_parse_fields_default_value_boolean() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "active").unwrap();
    field.set("type", "checkbox").unwrap();
    field.set("default_value", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].default_value, Some(JsonValue::Bool(true)));
}

#[test]
fn test_parse_fields_default_value_integer() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "count").unwrap();
    field.set("type", "number").unwrap();
    field.set("default_value", 42i64).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].default_value, Some(JsonValue::Number(42.into())));
}

#[test]
fn test_parse_fields_default_value_float() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "ratio").unwrap();
    field.set("type", "number").unwrap();
    field.set("default_value", 3.15f64).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    let dv = fields[0].default_value.as_ref().unwrap();
    assert!(dv.is_number());
}

#[test]
fn test_parse_fields_default_value_table_ignored() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "data").unwrap();
    field.set("type", "json").unwrap();
    let inner = lua.create_table().unwrap();
    field.set("default_value", inner).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(fields[0].default_value.is_none());
}

#[test]
fn test_parse_fields_group_with_subfields() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "meta").unwrap();
    field.set("type", "group").unwrap();
    let sub = lua.create_table().unwrap();
    let sf = lua.create_table().unwrap();
    sf.set("name", "title").unwrap();
    sf.set("type", "text").unwrap();
    sub.set(1, sf).unwrap();
    field.set("fields", sub).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].fields.len(), 1);
    assert_eq!(fields[0].fields[0].name, "title");
}

#[test]
fn test_parse_fields_row_with_subfields() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "layout_row").unwrap();
    field.set("type", "row").unwrap();
    let sub = lua.create_table().unwrap();
    let sf = lua.create_table().unwrap();
    sf.set("name", "first_name").unwrap();
    sf.set("type", "text").unwrap();
    sub.set(1, sf).unwrap();
    field.set("fields", sub).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].fields.len(), 1);
    assert_eq!(fields[0].fields[0].name, "first_name");
}

#[test]
fn test_parse_fields_collapsible_with_subfields() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "advanced").unwrap();
    field.set("type", "collapsible").unwrap();
    let sub = lua.create_table().unwrap();
    let sf = lua.create_table().unwrap();
    sf.set("name", "notes").unwrap();
    sf.set("type", "textarea").unwrap();
    sub.set(1, sf).unwrap();
    field.set("fields", sub).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].fields.len(), 1);
    assert_eq!(fields[0].fields[0].name, "notes");
}

#[test]
fn test_parse_fields_date_picker_appearance() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "published_at").unwrap();
    field.set("type", "date").unwrap();
    field.set("picker_appearance", "dayAndTime").unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(
        fields[0].picker_appearance,
        Some(PickerAppearance::DayAndTime)
    );
}

#[test]
fn test_parse_fields_date_picker_appearance_invalid_is_dropped() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "published_at").unwrap();
    field.set("type", "date").unwrap();
    field.set("picker_appearance", "datetime").unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    // Unknown value gets warned + dropped — see `parse_date_config`.
    assert!(fields[0].picker_appearance.is_none());
}

#[test]
fn test_parse_fields_non_date_picker_appearance_rejected() {
    // Strict per-type validation: `picker_appearance` is a date-only key,
    // so it is rejected (not silently ignored) on a text field.
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "title").unwrap();
    field.set("type", "text").unwrap();
    field.set("picker_appearance", "dayAndTime").unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err().to_string();
    assert!(err.contains("picker_appearance"), "{err}");
}

#[test]
fn test_parse_fields_min_max_float() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "score").unwrap();
    field.set("type", "number").unwrap();
    field.set("min", 0.0f64).unwrap();
    field.set("max", 100.0f64).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].min, Some(0.0));
    assert_eq!(fields[0].max, Some(100.0));
}

#[test]
fn test_parse_fields_min_max_integer() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "qty").unwrap();
    field.set("type", "number").unwrap();
    field.set("min", 1i64).unwrap();
    field.set("max", 99i64).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].min, Some(1.0));
    assert_eq!(fields[0].max, Some(99.0));
}

#[test]
fn test_parse_fields_join_type() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "authored_posts").unwrap();
    field.set("type", "join").unwrap();
    field.set("collection", "posts").unwrap();
    field.set("on", "author").unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    let join = fields[0].join.as_ref().unwrap();
    assert_eq!(join.collection, "posts");
    assert_eq!(join.on, "author");
}

#[test]
fn test_parse_fields_mcp_config() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "summary").unwrap();
    field.set("type", "text").unwrap();
    let mcp_tbl = lua.create_table().unwrap();
    mcp_tbl.set("description", "A short summary").unwrap();
    field.set("mcp", mcp_tbl).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(
        fields[0].mcp.description.as_deref(),
        Some("A short summary")
    );
}

#[test]
fn test_parse_fields_missing_name_returns_error() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("type", "text").unwrap();
    fields_tbl.set(1, field).unwrap();
    let result = parse_fields(&lua, &fields_tbl);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("missing 'name'"));
}

#[test]
fn test_parse_fields_array_min_max_rows() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "images").unwrap();
    field.set("type", "array").unwrap();
    field.set("min_rows", 1usize).unwrap();
    field.set("max_rows", 10usize).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].min_rows, Some(1));
    assert_eq!(fields[0].max_rows, Some(10));
}

#[test]
fn test_parse_fields_text_min_max_length() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "slug").unwrap();
    field.set("type", "text").unwrap();
    field.set("min_length", 3usize).unwrap();
    field.set("max_length", 64usize).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].min_length, Some(3));
    assert_eq!(fields[0].max_length, Some(64));
}

#[test]
fn test_parse_fields_date_min_max_date() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "birth_date").unwrap();
    field.set("type", "date").unwrap();
    field.set("min_date", "1900-01-01").unwrap();
    field.set("max_date", "2100-12-31").unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert_eq!(fields[0].min_date.as_deref(), Some("1900-01-01"));
    assert_eq!(fields[0].max_date.as_deref(), Some("2100-12-31"));
}

/// A time of day has no date for `min_date`/`max_date` to judge; the
/// bound used to reject every value at write time instead of the config
/// at load time.
#[test]
fn test_parse_fields_date_bounds_on_time_only_rejected() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "opens").unwrap();
    field.set("type", "date").unwrap();
    field.set("picker_appearance", "timeOnly").unwrap();
    field.set("min_date", "2024-01-01").unwrap();
    fields_tbl.set(1, field).unwrap();

    let err = parse_fields(&lua, &fields_tbl).unwrap_err().to_string();
    assert!(err.contains("timeOnly"), "{err}");
}

#[test]
fn test_parse_field_hooks_all_events() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "title").unwrap();
    field.set("type", "text").unwrap();
    let hooks_tbl = lua.create_table().unwrap();
    let bv = lua.create_table().unwrap();
    bv.set(1, "hooks.validate_title").unwrap();
    hooks_tbl.set("before_validate", bv).unwrap();
    let bc = lua.create_table().unwrap();
    bc.set(1, "hooks.transform_title").unwrap();
    hooks_tbl.set("before_change", bc).unwrap();
    let ac = lua.create_table().unwrap();
    ac.set(1, "hooks.after_title_change").unwrap();
    hooks_tbl.set("after_change", ac).unwrap();
    let ar = lua.create_table().unwrap();
    ar.set(1, "hooks.format_title").unwrap();
    hooks_tbl.set("after_read", ar).unwrap();
    field.set("hooks", hooks_tbl).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    let hooks = &fields[0].hooks;
    assert_eq!(
        hooks.before_validate,
        vec![HookRef::new("hooks.validate_title")]
    );
    assert_eq!(
        hooks.before_change,
        vec![HookRef::new("hooks.transform_title")]
    );
    assert_eq!(
        hooks.after_change,
        vec![HookRef::new("hooks.after_title_change")]
    );
    assert_eq!(hooks.after_read, vec![HookRef::new("hooks.format_title")]);
}

#[test]
fn test_parse_fields_min_exceeds_max_rejected() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "score").unwrap();
    field.set("type", "number").unwrap();
    field.set("min", 100.0f64).unwrap();
    field.set("max", 10.0f64).unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    assert!(err.to_string().contains("min"), "{}", err);
}

#[test]
fn test_parse_fields_min_length_exceeds_max_length_rejected() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "slug").unwrap();
    field.set("type", "text").unwrap();
    field.set("min_length", 100usize).unwrap();
    field.set("max_length", 10usize).unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    assert!(err.to_string().contains("min_length"), "{}", err);
}

#[test]
fn test_parse_fields_min_rows_exceeds_max_rows_rejected() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "items").unwrap();
    field.set("type", "array").unwrap();
    field.set("min_rows", 10usize).unwrap();
    field.set("max_rows", 3usize).unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    assert!(err.to_string().contains("min_rows"), "{}", err);
}

#[test]
fn test_parse_fields_date_timezone_enabled() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "event_at").unwrap();
    field.set("type", "date").unwrap();
    field.set("picker_appearance", "dayAndTime").unwrap();
    field.set("timezone", true).unwrap();
    field.set("default_timezone", "America/New_York").unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(fields[0].timezone, "timezone should be true");
    assert_eq!(
        fields[0].default_timezone.as_deref(),
        Some("America/New_York")
    );
}

#[test]
fn test_parse_fields_date_timezone_default_false() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "published_at").unwrap();
    field.set("type", "date").unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(!fields[0].timezone, "timezone should default to false");
    assert!(fields[0].default_timezone.is_none());
}

#[test]
fn test_parse_fields_timezone_ignored_for_day_only() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "birthday").unwrap();
    field.set("type", "date").unwrap();
    field.set("picker_appearance", "dayOnly").unwrap();
    field.set("timezone", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(
        !fields[0].timezone,
        "timezone should be ignored for dayOnly"
    );
}

#[test]
fn test_parse_fields_timezone_ignored_for_default_appearance() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "birthday").unwrap();
    field.set("type", "date").unwrap();
    // No picker_appearance set — defaults to dayOnly
    field.set("timezone", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(
        !fields[0].timezone,
        "timezone should be ignored when picker_appearance defaults to dayOnly"
    );
}

/// `dayAndTime` is the only appearance that carries a zone, so `monthOnly`
/// drops it like the other three. Every display path relies on this: a
/// month-only value is a UTC calendar value the list and the form both show
/// as stored, and a `_tz` companion would make them disagree.
#[test]
fn test_parse_fields_timezone_ignored_for_month_only() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "billing_period").unwrap();
    field.set("type", "date").unwrap();
    field.set("picker_appearance", "monthOnly").unwrap();
    field.set("timezone", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(
        !fields[0].timezone,
        "timezone should be ignored for monthOnly"
    );
}

#[test]
fn test_parse_fields_timezone_ignored_for_time_only() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "alarm").unwrap();
    field.set("type", "date").unwrap();
    field.set("picker_appearance", "timeOnly").unwrap();
    field.set("timezone", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let fields = parse_fields(&lua, &fields_tbl).unwrap();
    assert!(
        !fields[0].timezone,
        "timezone should be ignored for timeOnly"
    );
}

#[test]
fn test_parse_fields_timezone_rejected_for_non_date() {
    // Strict per-type validation: `timezone` is a date-only key, so it is
    // rejected (not silently ignored) on a text field.
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "title").unwrap();
    field.set("type", "text").unwrap();
    field.set("timezone", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err().to_string();
    assert!(err.contains("timezone"), "{err}");
}

/// Regression: boolean default on a text field must be rejected.
#[test]
fn test_parse_fields_default_value_type_mismatch_text_boolean() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "title").unwrap();
    field.set("type", "text").unwrap();
    field.set("default_value", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    assert!(
        err.to_string().contains("default_value type mismatch"),
        "Expected type mismatch error: {err}",
    );
}

/// Regression: string default on a number field must be rejected.
#[test]
fn test_parse_fields_default_value_type_mismatch_number_string() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "count").unwrap();
    field.set("type", "number").unwrap();
    field.set("default_value", "not-a-number").unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    assert!(
        err.to_string().contains("default_value type mismatch"),
        "Expected type mismatch error: {err}",
    );
}

/// Regression: number default on a checkbox must be rejected.
#[test]
fn test_parse_fields_default_value_type_mismatch_checkbox_number() {
    let lua = Lua::new();
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "active").unwrap();
    field.set("type", "checkbox").unwrap();
    field.set("default_value", 42i64).unwrap();
    fields_tbl.set(1, field).unwrap();
    let err = parse_fields(&lua, &fields_tbl).unwrap_err();
    assert!(
        err.to_string().contains("default_value type mismatch"),
        "Expected type mismatch error: {err}",
    );
}

/// Correct type combinations should still pass.
#[test]
fn test_parse_fields_default_value_correct_types_pass() {
    let lua = Lua::new();

    // Boolean default on checkbox — OK
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "active").unwrap();
    field.set("type", "checkbox").unwrap();
    field.set("default_value", true).unwrap();
    fields_tbl.set(1, field).unwrap();
    assert!(parse_fields(&lua, &fields_tbl).is_ok());

    // String default on text — OK
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "title").unwrap();
    field.set("type", "text").unwrap();
    field.set("default_value", "hello").unwrap();
    fields_tbl.set(1, field).unwrap();
    assert!(parse_fields(&lua, &fields_tbl).is_ok());

    // Number default on number — OK
    let fields_tbl = lua.create_table().unwrap();
    let field = lua.create_table().unwrap();
    field.set("name", "count").unwrap();
    field.set("type", "number").unwrap();
    field.set("default_value", 10i64).unwrap();
    fields_tbl.set(1, field).unwrap();
    assert!(parse_fields(&lua, &fields_tbl).is_ok());
}
