//! Parsing functions for field admin configuration.

use mlua::{Error::RuntimeError, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;

use crate::{
    core::{FieldAdmin, FieldAdminBuilder, validate_template_name},
    hooks::lua_api::lua_to_json,
};

use super::helpers::{
    deny_unknown_keys, get_bool, get_localized_string, get_optional_hook_ref, get_string, get_table,
};

/// Every key accepted in a field's `admin = {...}` sub-table. Mirrors the
/// `FieldAdmin` struct (Lua key names). Plugin/custom-template config goes
/// under `extra` — the one open-ended escape hatch — so the rest can be
/// strictly validated, rejecting typos like `lable` or `readony`.
const FIELD_ADMIN_KEYS: &[&str] = &[
    "label",
    "placeholder",
    "description",
    "hidden",
    "readonly",
    "width",
    "collapsed",
    "label_field",
    "row_label",
    "labels",
    "position",
    "condition",
    "step",
    "rows",
    "language",
    "languages",
    "features",
    "picker",
    "format",
    "nodes",
    "resizable",
    "template",
    "extra",
];

/// Parse the `admin` subtable of a field Lua definition into a `FieldAdmin`.
///
/// Sections (boolean flags, localized strings, identifier strings, the
/// `labels` sub-table, the `rows` numeric knob, sequence-typed lists, the
/// template-ref string, the freeform `extra` map) are applied in turn by
/// per-section helpers.
pub(super) fn parse_field_admin(admin_tbl: &Table) -> LuaResult<FieldAdmin> {
    deny_unknown_keys(admin_tbl, "field admin", FIELD_ADMIN_KEYS)
        .map_err(|e| RuntimeError(e.to_string()))?;

    let mut builder = parse_admin_booleans(admin_tbl)?;
    builder = apply_localized_strings(builder, admin_tbl);
    builder = apply_identifier_strings(builder, admin_tbl)?;
    builder = apply_label_overrides(builder, admin_tbl);
    builder = apply_rows(builder, admin_tbl);
    builder = apply_sequence_lists(builder, admin_tbl);
    builder = apply_template(builder, admin_tbl)?;
    builder = apply_extra(builder, admin_tbl)?;
    Ok(builder.build())
}

/// Seed the builder with the four boolean flags (`collapsed`, `hidden`,
/// `readonly`, `resizable`). `collapsed`/`resizable` default to `true`;
/// `hidden`/`readonly` default to `false`.
fn parse_admin_booleans(admin_tbl: &Table) -> LuaResult<FieldAdminBuilder> {
    Ok(FieldAdmin::builder()
        .collapsed(get_bool(admin_tbl, "collapsed", true)?)
        .hidden(get_bool(admin_tbl, "hidden", false)?)
        .readonly(get_bool(admin_tbl, "readonly", false)?)
        .resizable(get_bool(admin_tbl, "resizable", true)?))
}

/// Apply the three localized-string fields (`label`, `placeholder`,
/// `description`). Each is optional.
fn apply_localized_strings(mut builder: FieldAdminBuilder, admin_tbl: &Table) -> FieldAdminBuilder {
    if let Some(v) = get_localized_string(admin_tbl, "label") {
        builder = builder.label(v);
    }
    if let Some(v) = get_localized_string(admin_tbl, "placeholder") {
        builder = builder.placeholder(v);
    }
    if let Some(v) = get_localized_string(admin_tbl, "description") {
        builder = builder.description(v);
    }
    builder
}

/// Apply the optional plain-string admin knobs (`width`, `label_field`,
/// `row_label`, `position`, `condition`, `step`, `language`, `picker`,
/// `format`).
fn apply_identifier_strings(
    mut builder: FieldAdminBuilder,
    admin_tbl: &Table,
) -> LuaResult<FieldAdminBuilder> {
    if let Some(v) = get_string(admin_tbl, "width") {
        builder = builder.width(v);
    }
    if let Some(v) = get_string(admin_tbl, "label_field") {
        builder = builder.label_field(v);
    }
    if let Some(v) = get_string(admin_tbl, "row_label") {
        builder = builder.row_label(v);
    }
    if let Some(v) = get_string(admin_tbl, "position") {
        builder = builder.position(v);
    }
    if let Some(v) = get_optional_hook_ref(admin_tbl, "condition", "admin condition")
        .map_err(|e| RuntimeError(e.to_string()))?
    {
        builder = builder.condition(v);
    }
    if let Some(v) = get_string(admin_tbl, "step") {
        builder = builder.step(v);
    }
    if let Some(v) = get_string(admin_tbl, "language") {
        builder = builder.language(v);
    }
    if let Some(v) = get_string(admin_tbl, "picker") {
        builder = builder.picker(v);
    }
    if let Some(v) = get_string(admin_tbl, "format") {
        builder = builder.richtext_format(v);
    }
    Ok(builder)
}

/// Apply the singular/plural label overrides from the `labels` sub-table.
fn apply_label_overrides(mut builder: FieldAdminBuilder, admin_tbl: &Table) -> FieldAdminBuilder {
    let Ok(labels_tbl) = get_table(admin_tbl, "labels") else {
        return builder;
    };
    if let Some(v) = get_localized_string(&labels_tbl, "singular") {
        builder = builder.labels_singular(v);
    }
    if let Some(v) = get_localized_string(&labels_tbl, "plural") {
        builder = builder.labels_plural(v);
    }
    builder
}

/// Apply the optional `rows: u32` textarea knob.
fn apply_rows(mut builder: FieldAdminBuilder, admin_tbl: &Table) -> FieldAdminBuilder {
    if let Some(v) = admin_tbl.get::<Option<u32>>("rows").ok().flatten() {
        builder = builder.rows(v);
    }
    builder
}

/// Apply the three sequence-typed lists (`languages`, `features`, `nodes`).
/// Each is always set on the builder; an absent or invalid Lua sequence
/// becomes an empty Vec.
fn apply_sequence_lists(mut builder: FieldAdminBuilder, admin_tbl: &Table) -> FieldAdminBuilder {
    builder = builder.languages(string_sequence(admin_tbl, "languages"));
    builder = builder.features(string_sequence(admin_tbl, "features"));
    builder = builder.nodes(string_sequence(admin_tbl, "nodes"));
    builder
}

/// Collect a Lua sequence at `key` into a `Vec<String>`. Returns an empty
/// vec when the key is absent or the value isn't a sequence of strings.
fn string_sequence(admin_tbl: &Table, key: &str) -> Vec<String> {
    let Ok(tbl) = get_table(admin_tbl, key) else {
        return Vec::new();
    };
    tbl.sequence_values::<String>()
        .filter_map(std::result::Result::ok)
        .collect()
}

/// Apply the optional `template` ref, validating it against the allowed
/// template-name shape before storing.
fn apply_template(
    mut builder: FieldAdminBuilder,
    admin_tbl: &Table,
) -> LuaResult<FieldAdminBuilder> {
    if let Some(v) = get_string(admin_tbl, "template") {
        validate_template_name(&v)
            .map_err(|e| RuntimeError(format!("crap.fields.*: invalid `admin.template`: {e}")))?;
        builder = builder.template(v);
    }
    Ok(builder)
}

/// Apply the freeform `admin.extra` map — JSON-serializable values the
/// field's template can read at `{{admin.extra.<key>}}`. Parsed once at
/// field-definition time; static per field instance.
fn apply_extra(mut builder: FieldAdminBuilder, admin_tbl: &Table) -> LuaResult<FieldAdminBuilder> {
    let Ok(extra_tbl) = get_table(admin_tbl, "extra") else {
        return Ok(builder);
    };
    let json = lua_to_json(&Value::Table(extra_tbl)).map_err(|e| {
        RuntimeError(format!(
            "crap.fields.*: invalid `admin.extra` (must be JSON-serializable): {e}"
        ))
    })?;
    match json {
        JsonValue::Object(map) => {
            builder = builder.extra(map);
            Ok(builder)
        }
        _ => Err(RuntimeError(
            "crap.fields.*: `admin.extra` must be a table (Lua dictionary), \
             not a sequence or scalar"
                .to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_parse_field_admin_labels_features_nodes() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let labels_tbl = lua.create_table().unwrap();
        labels_tbl.set("singular", "Item").unwrap();
        labels_tbl.set("plural", "Items").unwrap();
        admin_tbl.set("labels", labels_tbl).unwrap();
        let features = lua.create_table().unwrap();
        features.set(1, "bold").unwrap();
        features.set(2, "italic").unwrap();
        admin_tbl.set("features", features).unwrap();
        let nodes = lua.create_table().unwrap();
        nodes.set(1, "paragraph").unwrap();
        admin_tbl.set("nodes", nodes).unwrap();
        admin_tbl.set("format", "lexical").unwrap();
        admin_tbl.set("language", "en").unwrap();
        admin_tbl.set("rows", 5u32).unwrap();
        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert!(admin.labels.singular.is_some());
        assert!(admin.labels.plural.is_some());
        assert_eq!(admin.features, vec!["bold", "italic"]);
        assert_eq!(admin.nodes, vec!["paragraph"]);
        assert_eq!(admin.richtext_format.as_deref(), Some("lexical"));
        assert_eq!(admin.language.as_deref(), Some("en"));
        assert_eq!(admin.rows, Some(5));
        assert!(admin.resizable);
    }

    #[test]
    fn test_parse_field_admin_languages_array() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let langs = lua.create_table().unwrap();
        langs.set(1, "javascript").unwrap();
        langs.set(2, "python").unwrap();
        langs.set(3, "html").unwrap();
        admin_tbl.set("languages", langs).unwrap();

        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert_eq!(admin.languages, vec!["javascript", "python", "html"]);
    }

    #[test]
    fn test_parse_field_admin_languages_default_empty() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert!(admin.languages.is_empty());
    }

    #[test]
    fn test_parse_field_admin_resizable_false() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("resizable", false).unwrap();
        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert!(!admin.resizable);
    }

    #[test]
    fn test_parse_field_admin_template_safe_path_accepted() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("template", "fields/rating").unwrap();
        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert_eq!(admin.template.as_deref(), Some("fields/rating"));
    }

    #[test]
    fn test_parse_field_admin_template_unsafe_path_rejected() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("template", "../../etc/passwd").unwrap();
        let err = parse_field_admin(&admin_tbl).unwrap_err();
        assert!(
            err.to_string().contains("invalid `admin.template`"),
            "expected validation error, got: {err}"
        );
    }

    #[test]
    fn test_parse_field_admin_extra_accepts_scalar_and_nested() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let extra = lua.create_table().unwrap();
        extra.set("icon", "star").unwrap();
        extra.set("max_stars", 5i64).unwrap();
        extra.set("rounded", true).unwrap();
        let nested = lua.create_table().unwrap();
        nested.set("primary", "#1677ff").unwrap();
        nested.set("secondary", "#52c41a").unwrap();
        extra.set("colors", nested).unwrap();
        admin_tbl.set("extra", extra).unwrap();

        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert_eq!(
            admin.extra.get("icon").and_then(|v| v.as_str()),
            Some("star")
        );
        assert_eq!(
            admin
                .extra
                .get("max_stars")
                .and_then(serde_json::Value::as_i64),
            Some(5)
        );
        assert_eq!(
            admin
                .extra
                .get("rounded")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        let colors = admin.extra.get("colors").and_then(|v| v.as_object());
        assert!(colors.is_some(), "nested object preserved");
        assert_eq!(
            colors.unwrap().get("primary").and_then(|v| v.as_str()),
            Some("#1677ff")
        );
    }

    #[test]
    fn test_parse_field_admin_extra_rejects_array_value() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        // A sequence (1-indexed numeric keys) should be rejected — extra
        // is meant to be a key/value config map.
        let arr = lua.create_table().unwrap();
        arr.set(1, "first").unwrap();
        arr.set(2, "second").unwrap();
        admin_tbl.set("extra", arr).unwrap();

        let err = parse_field_admin(&admin_tbl).unwrap_err();
        assert!(
            err.to_string().contains("must be a table"),
            "expected sequence-rejection error, got: {err}"
        );
    }

    #[test]
    fn test_parse_field_admin_extra_default_empty() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert!(admin.extra.is_empty());
    }
}
