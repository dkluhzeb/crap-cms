//! Parsing functions for field admin configuration.

use crate::hooks::lua_api::utils::lua_err;
use mlua::{Error::RuntimeError, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;

use crate::{
    core::{
        FieldAdmin, FieldAdminBuilder, FieldType,
        richtext::{RICHTEXT_FEATURES, validate_node_name},
        validate_template_name,
    },
    hooks::lua_api::lua_to_json,
};

use super::helpers::{
    deny_unknown_keys, get_bool, get_localized_string_strict, get_optional_hook_ref,
    get_optional_table, get_string_sequence, get_string_strict,
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

/// The `admin` keys only some field types read, each with the types that read
/// it — in the edit form, or in the rich text node-attribute modal for the
/// scalar types a node attr may use. Every other key applies to every field
/// type. On any type not listed a key would be silently inert, so it is
/// refused there.
const TYPE_SCOPED_ADMIN_KEYS: &[(&str, &[FieldType])] = &[
    (
        "placeholder",
        &[
            FieldType::Text,
            FieldType::Email,
            FieldType::Number,
            FieldType::Textarea,
            FieldType::Json,
            FieldType::Code,
            FieldType::Richtext,
        ],
    ),
    (
        "collapsed",
        &[
            FieldType::Group,
            FieldType::Collapsible,
            FieldType::Array,
            FieldType::Blocks,
        ],
    ),
    ("label_field", &[FieldType::Array, FieldType::Blocks]),
    ("row_label", &[FieldType::Array, FieldType::Blocks]),
    ("labels", &[FieldType::Array, FieldType::Blocks]),
    ("step", &[FieldType::Number]),
    (
        "rows",
        &[FieldType::Textarea, FieldType::Code, FieldType::Json],
    ),
    ("language", &[FieldType::Code]),
    ("languages", &[FieldType::Code]),
    (
        "picker",
        &[
            FieldType::Relationship,
            FieldType::Upload,
            FieldType::Blocks,
        ],
    ),
    ("resizable", &[FieldType::Textarea, FieldType::Richtext]),
    ("format", &[FieldType::Richtext]),
    ("features", &[FieldType::Richtext]),
    ("nodes", &[FieldType::Richtext]),
];

/// The `admin.picker` values a field type reads; empty for a type without a
/// picker.
fn picker_values(field_type: &FieldType) -> &'static [&'static str] {
    match field_type {
        FieldType::Blocks => &["select", "card"],
        FieldType::Upload | FieldType::Relationship => &["drawer", "none"],
        _ => &[],
    }
}

/// Refuse an `admin` key on a field type that does not read it (see
/// [`TYPE_SCOPED_ADMIN_KEYS`]), and an `admin.picker` value the field's type
/// does not offer.
pub(super) fn deny_type_scoped_admin_keys(
    admin_tbl: &Table,
    field_type: &FieldType,
) -> LuaResult<()> {
    for (key, types) in TYPE_SCOPED_ADMIN_KEYS {
        if types.contains(field_type) || !admin_tbl.contains_key(*key)? {
            continue;
        }

        let names: Vec<&str> = types.iter().map(FieldType::as_str).collect();

        return Err(RuntimeError(format!(
            "field admin '{key}' has no effect on a {} field — it applies only to: {}",
            field_type.as_str(),
            names.join(", ")
        )));
    }

    let Some(picker) = admin_tbl.get::<Option<String>>("picker").ok().flatten() else {
        return Ok(());
    };

    check_admin_enum("picker", &picker, picker_values(field_type))
}

/// Parse the `admin` subtable of a field Lua definition into a `FieldAdmin`.
///
/// Sections (boolean flags, localized strings, identifier strings, the
/// `labels` sub-table, the `rows` numeric knob, sequence-typed lists, the
/// template-ref string, the freeform `extra` map) are applied in turn by
/// per-section helpers.
pub(super) fn parse_field_admin(admin_tbl: &Table) -> LuaResult<FieldAdmin> {
    deny_unknown_keys(admin_tbl, "field admin", FIELD_ADMIN_KEYS).map_err(lua_err)?;

    let mut builder = parse_admin_booleans(admin_tbl)?;
    builder = apply_localized_strings(builder, admin_tbl)?;
    builder = apply_identifier_strings(builder, admin_tbl)?;
    builder = apply_label_overrides(builder, admin_tbl)?;
    builder = apply_rows(builder, admin_tbl)?;
    builder = apply_sequence_lists(builder, admin_tbl)?;
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
/// `description`). Each is optional; a present-but-wrong-typed value is a
/// hard error.
fn apply_localized_strings(
    mut builder: FieldAdminBuilder,
    admin_tbl: &Table,
) -> LuaResult<FieldAdminBuilder> {
    if let Some(v) = get_localized_string_strict(admin_tbl, "label", "field admin")? {
        builder = builder.label(v);
    }
    if let Some(v) = get_localized_string_strict(admin_tbl, "placeholder", "field admin")? {
        builder = builder.placeholder(v);
    }
    if let Some(v) = get_localized_string_strict(admin_tbl, "description", "field admin")? {
        builder = builder.description(v);
    }
    Ok(builder)
}

/// Apply the optional plain-string admin knobs (`width`, `label_field`,
/// `row_label`, `position`, `condition`, `step`, `language`, `picker`,
/// `format`).
fn apply_identifier_strings(
    mut builder: FieldAdminBuilder,
    admin_tbl: &Table,
) -> LuaResult<FieldAdminBuilder> {
    if let Some(v) = get_string_strict(admin_tbl, "width", "field admin")? {
        builder = builder.width(v);
    }
    if let Some(v) = get_string_strict(admin_tbl, "label_field", "field admin")? {
        builder = builder.label_field(v);
    }
    if let Some(v) = get_string_strict(admin_tbl, "row_label", "field admin")? {
        builder = builder.row_label(v);
    }
    if let Some(v) = get_string_strict(admin_tbl, "position", "field admin")? {
        check_admin_enum("position", &v, &["main", "sidebar"])?;
        builder = builder.position(v);
    }
    if let Some(v) =
        get_optional_hook_ref(admin_tbl, "condition", "admin condition").map_err(lua_err)?
    {
        builder = builder.condition(v);
    }
    if let Some(v) = get_string_strict(admin_tbl, "step", "field admin")? {
        builder = builder.step(v);
    }
    if let Some(v) = get_string_strict(admin_tbl, "language", "field admin")? {
        builder = builder.language(v);
    }
    if let Some(v) = get_string_strict(admin_tbl, "picker", "field admin")? {
        check_admin_enum("picker", &v, &["select", "card", "drawer", "none"])?;
        builder = builder.picker(v);
    }
    if let Some(v) = get_string_strict(admin_tbl, "format", "field admin")? {
        check_admin_enum("format", &v, &["html", "json"])?;
        builder = builder.richtext_format(v);
    }
    Ok(builder)
}

/// Reject a present-but-unrecognized value for an enum-typed `admin.*` key.
/// Previously any string was accepted and silently defaulted downstream — a
/// typo (`position = "sidbar"`, `format = "lexical"`) is now a load error.
fn check_admin_enum(key: &str, value: &str, allowed: &[&str]) -> LuaResult<()> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(RuntimeError(format!(
        "field admin '{key}': unknown value '{value}'. Valid values: {}",
        allowed.join(", ")
    )))
}

/// Apply the singular/plural label overrides from the `labels` sub-table.
/// A present-but-non-table `labels`, an unknown key inside it, or a
/// wrong-typed value is a hard error.
fn apply_label_overrides(
    mut builder: FieldAdminBuilder,
    admin_tbl: &Table,
) -> LuaResult<FieldAdminBuilder> {
    let Some(labels_tbl) = get_optional_table(admin_tbl, "labels", "field admin")? else {
        return Ok(builder);
    };

    deny_unknown_keys(&labels_tbl, "field admin labels", &["singular", "plural"])
        .map_err(lua_err)?;

    if let Some(v) = get_localized_string_strict(&labels_tbl, "singular", "field admin labels")? {
        builder = builder.labels_singular(v);
    }
    if let Some(v) = get_localized_string_strict(&labels_tbl, "plural", "field admin labels")? {
        builder = builder.labels_plural(v);
    }
    Ok(builder)
}

/// Apply the optional `rows: u32` textarea knob. A present value that is not
/// a non-negative integer is a hard error.
fn apply_rows(mut builder: FieldAdminBuilder, admin_tbl: &Table) -> LuaResult<FieldAdminBuilder> {
    let rows = admin_tbl.get::<Option<u32>>("rows").map_err(|e| {
        RuntimeError(format!(
            "field admin 'rows' must be a non-negative integer: {e}"
        ))
    })?;

    if let Some(v) = rows {
        builder = builder.rows(v);
    }
    Ok(builder)
}

/// Apply the three sequence-typed lists (`languages`, `features`, `nodes`).
/// Each is always set on the builder; absent → empty Vec, but a present
/// non-table value or a non-string entry is a hard error. Every `features`
/// entry must be a known editor feature, and every `nodes` entry a valid
/// custom node name (whether it is registered is checked once all
/// definitions have loaded).
fn apply_sequence_lists(
    mut builder: FieldAdminBuilder,
    admin_tbl: &Table,
) -> LuaResult<FieldAdminBuilder> {
    let features = get_string_sequence(admin_tbl, "features", "field admin")?;
    let nodes = get_string_sequence(admin_tbl, "nodes", "field admin")?;

    for feature in &features {
        check_admin_enum("features", feature, RICHTEXT_FEATURES)?;
    }

    for node in &nodes {
        validate_node_name(node).map_err(|e| RuntimeError(format!("field admin 'nodes': {e}")))?;
    }

    builder = builder.languages(get_string_sequence(admin_tbl, "languages", "field admin")?);
    builder = builder.features(features);
    builder = builder.nodes(nodes);
    Ok(builder)
}

/// Apply the optional `template` ref, validating it against the allowed
/// template-name shape before storing.
fn apply_template(
    mut builder: FieldAdminBuilder,
    admin_tbl: &Table,
) -> LuaResult<FieldAdminBuilder> {
    if let Some(v) = get_string_strict(admin_tbl, "template", "field admin")? {
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
    let Some(extra_tbl) = get_optional_table(admin_tbl, "extra", "field admin")? else {
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
        nodes.set(1, "cta").unwrap();
        admin_tbl.set("nodes", nodes).unwrap();
        admin_tbl.set("format", "json").unwrap();
        admin_tbl.set("language", "en").unwrap();
        admin_tbl.set("rows", 5u32).unwrap();
        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert!(admin.labels.singular.is_some());
        assert!(admin.labels.plural.is_some());
        assert_eq!(admin.features, vec!["bold", "italic"]);
        assert_eq!(admin.nodes, vec!["cta"]);
        assert_eq!(admin.richtext_format.as_deref(), Some("json"));
        assert_eq!(admin.language.as_deref(), Some("en"));
        assert_eq!(admin.rows, Some(5));
        assert!(admin.resizable);
    }

    /// Regression: enum-typed `admin.*` values are validated — a typo
    /// (`format = "lexical"`, `position = "sidbar"`, `picker = "grid"`) is a
    /// load error, not silently stored-and-ignored.
    #[test]
    fn admin_enum_values_are_validated() {
        let lua = Lua::new();
        for (key, bad) in [
            ("position", "sidbar"),
            ("picker", "grid"),
            ("format", "lexical"),
        ] {
            let admin_tbl = lua.create_table().unwrap();
            admin_tbl.set(key, bad).unwrap();
            let err = parse_field_admin(&admin_tbl).unwrap_err().to_string();
            assert!(
                err.contains(key) && err.contains(bad),
                "expected {key} rejection for '{bad}', got: {err}"
            );
        }
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

    /// Assert that `parse_field_admin` rejects the given table with an error
    /// mentioning `needle`.
    fn assert_rejected(admin_tbl: &Table, needle: &str) {
        let err = parse_field_admin(admin_tbl).unwrap_err().to_string();
        assert!(
            err.contains(needle),
            "expected error containing {needle:?}, got: {err}"
        );
    }

    #[test]
    fn rejects_non_string_width() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("width", true).unwrap();
        assert_rejected(&admin_tbl, "'width' must be a string");
    }

    #[test]
    fn rejects_non_string_label() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("label", false).unwrap();
        assert_rejected(&admin_tbl, "'label' must be a string");
    }

    #[test]
    fn rejects_empty_localized_label_table() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("label", lua.create_table().unwrap()).unwrap();
        assert_rejected(&admin_tbl, "must not be empty");
    }

    #[test]
    fn rejects_non_table_labels() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("labels", "Item").unwrap();
        assert_rejected(&admin_tbl, "'labels' must be a table");
    }

    #[test]
    fn rejects_unknown_labels_key() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let labels_tbl = lua.create_table().unwrap();
        labels_tbl.set("singular", "Item").unwrap();
        labels_tbl.set("plurral", "Items").unwrap();
        admin_tbl.set("labels", labels_tbl).unwrap();
        assert_rejected(&admin_tbl, "plurral");
    }

    #[test]
    fn rejects_non_integer_rows() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("rows", true).unwrap();
        assert_rejected(&admin_tbl, "'rows' must be a non-negative integer");
    }

    #[test]
    fn rejects_non_table_features() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("features", "bold").unwrap();
        assert_rejected(&admin_tbl, "'features' must be a table");
    }

    #[test]
    fn rejects_non_string_feature_entry() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let features = lua.create_table().unwrap();
        features.set(1, "bold").unwrap();
        features.set(2, true).unwrap();
        admin_tbl.set("features", features).unwrap();
        assert_rejected(&admin_tbl, "'features' must be an array of strings");
    }

    /// Regression: `features` entries were never checked, so a typo
    /// (`bulletlist`) or an unsupported name (`underline`) booted fine and the
    /// toolbar silently lacked it.
    #[test]
    fn rejects_unknown_feature() {
        let lua = Lua::new();
        for bad in ["bulletlist", "underline"] {
            let admin_tbl = lua.create_table().unwrap();
            let features = lua.create_table().unwrap();
            features.set(1, "bold").unwrap();
            features.set(2, bad).unwrap();
            admin_tbl.set("features", features).unwrap();
            assert_rejected(&admin_tbl, bad);
        }
    }

    /// Regression: `nodes` accepted any string — a built-in node name
    /// (`paragraph`, enabled through `features`, never a custom node) or a
    /// misspelt one (`CTA`) — and the node was silently dropped.
    #[test]
    fn rejects_invalid_node_names() {
        let lua = Lua::new();
        for bad in ["paragraph", "CTA", "my-node"] {
            let admin_tbl = lua.create_table().unwrap();
            let nodes = lua.create_table().unwrap();
            nodes.set(1, bad).unwrap();
            admin_tbl.set("nodes", nodes).unwrap();
            assert_rejected(&admin_tbl, bad);
        }
    }

    /// Regression: type-specific `admin` keys (`format`, `features`,
    /// `nodes`, `picker`, `rows`, `step`, `language`, …) parsed on any field
    /// type and were silently inert off the types that read them.
    #[test]
    fn type_scoped_admin_keys_are_refused_on_other_field_types() {
        let lua = Lua::new();

        for (key, types) in TYPE_SCOPED_ADMIN_KEYS {
            let admin_tbl = lua.create_table().unwrap();
            admin_tbl.set(*key, lua.create_table().unwrap()).unwrap();

            let err = deny_type_scoped_admin_keys(&admin_tbl, &FieldType::Checkbox)
                .unwrap_err()
                .to_string();
            assert!(err.contains(key) && err.contains("checkbox"), "{err}");

            for field_type in *types {
                let scoped = lua.create_table().unwrap();
                scoped.set(*key, true).unwrap();
                if *key == "picker" {
                    scoped.set(*key, picker_values(field_type)[0]).unwrap();
                }
                assert!(
                    deny_type_scoped_admin_keys(&scoped, field_type).is_ok(),
                    "{key} on {}",
                    field_type.as_str()
                );
            }
        }
    }

    /// Keys every field type reads are accepted everywhere.
    #[test]
    fn generic_admin_keys_are_accepted_on_every_type() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("label", "L").unwrap();
        admin_tbl.set("readonly", true).unwrap();
        admin_tbl.set("hidden", true).unwrap();

        assert!(deny_type_scoped_admin_keys(&admin_tbl, &FieldType::Checkbox).is_ok());
    }

    /// Each picker type offers its own values: `card` is a blocks picker, not
    /// an upload one.
    #[test]
    fn picker_values_are_checked_per_type() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("picker", "card").unwrap();

        assert!(deny_type_scoped_admin_keys(&admin_tbl, &FieldType::Blocks).is_ok());

        let err = deny_type_scoped_admin_keys(&admin_tbl, &FieldType::Upload)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'card'") && err.contains("drawer, none"),
            "{err}"
        );
    }

    #[test]
    fn rejects_non_string_template() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("template", true).unwrap();
        assert_rejected(&admin_tbl, "'template' must be a string");
    }

    #[test]
    fn rejects_non_table_extra() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("extra", "star").unwrap();
        assert_rejected(&admin_tbl, "'extra' must be a table");
    }
}
