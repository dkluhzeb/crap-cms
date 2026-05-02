//! Parsing functions for field admin configuration.

use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;

use crate::{
    core::{FieldAdmin, validate_template_name},
    hooks::api::lua_to_json,
};

use super::helpers::*;

/// Parse the `admin` subtable of a field Lua definition into a `FieldAdmin`.
pub(super) fn parse_field_admin(lua: &Lua, admin_tbl: &Table) -> LuaResult<FieldAdmin> {
    let (labels_singular, labels_plural) = if let Ok(labels_tbl) = get_table(admin_tbl, "labels") {
        (
            get_localized_string(&labels_tbl, "singular"),
            get_localized_string(&labels_tbl, "plural"),
        )
    } else {
        (None, None)
    };

    let mut builder = FieldAdmin::builder()
        .collapsed(get_bool(admin_tbl, "collapsed", true)?)
        .hidden(get_bool(admin_tbl, "hidden", false)?)
        .readonly(get_bool(admin_tbl, "readonly", false)?)
        .resizable(get_bool(admin_tbl, "resizable", true)?);

    if let Some(v) = get_localized_string(admin_tbl, "label") {
        builder = builder.label(v);
    }

    if let Some(v) = get_localized_string(admin_tbl, "placeholder") {
        builder = builder.placeholder(v);
    }

    if let Some(v) = get_localized_string(admin_tbl, "description") {
        builder = builder.description(v);
    }

    if let Some(v) = get_string(admin_tbl, "width") {
        builder = builder.width(v);
    }

    if let Some(v) = get_string(admin_tbl, "label_field") {
        builder = builder.label_field(v);
    }

    if let Some(v) = get_string(admin_tbl, "row_label") {
        builder = builder.row_label(v);
    }

    if let Some(v) = labels_singular {
        builder = builder.labels_singular(v);
    }

    if let Some(v) = labels_plural {
        builder = builder.labels_plural(v);
    }

    if let Some(v) = get_string(admin_tbl, "position") {
        builder = builder.position(v);
    }

    if let Some(v) = get_string(admin_tbl, "condition") {
        builder = builder.condition(v);
    }

    if let Some(v) = get_string(admin_tbl, "step") {
        builder = builder.step(v);
    }

    if let Some(v) = admin_tbl.get::<Option<u32>>("rows").ok().flatten() {
        builder = builder.rows(v);
    }

    if let Some(v) = get_string(admin_tbl, "language") {
        builder = builder.language(v);
    }

    let languages: Vec<String> = if let Ok(tbl) = get_table(admin_tbl, "languages") {
        tbl.sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect()
    } else {
        Vec::new()
    };

    builder = builder.languages(languages);

    if let Some(v) = get_string(admin_tbl, "picker") {
        builder = builder.picker(v);
    }

    if let Some(v) = get_string(admin_tbl, "format") {
        builder = builder.richtext_format(v);
    }

    let features: Vec<String> = if let Ok(tbl) = get_table(admin_tbl, "features") {
        tbl.sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect()
    } else {
        Vec::new()
    };

    builder = builder.features(features);

    let nodes: Vec<String> = if let Ok(tbl) = get_table(admin_tbl, "nodes") {
        tbl.sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect()
    } else {
        Vec::new()
    };

    builder = builder.nodes(nodes);

    if let Some(v) = get_string(admin_tbl, "template") {
        validate_template_name(&v)
            .map_err(|e| RuntimeError(format!("crap.fields.*: invalid `admin.template`: {e}")))?;
        builder = builder.template(v);
    }

    // Freeform per-field config — JSON-serializable values the field's
    // template can read at `{{admin.extra.<key>}}`. Parsed once at
    // field-definition time; static per field instance.
    if let Ok(extra_tbl) = get_table(admin_tbl, "extra") {
        let json = lua_to_json(lua, &Value::Table(extra_tbl)).map_err(|e| {
            RuntimeError(format!(
                "crap.fields.*: invalid `admin.extra` (must be JSON-serializable): {e}"
            ))
        })?;
        match json {
            JsonValue::Object(map) => builder = builder.extra(map),
            _ => {
                return Err(RuntimeError(
                    "crap.fields.*: `admin.extra` must be a table (Lua dictionary), \
                     not a sequence or scalar"
                        .to_string(),
                ));
            }
        }
    }

    Ok(builder.build())
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
        let admin = parse_field_admin(&lua, &admin_tbl).unwrap();
        assert!(admin.labels_singular.is_some());
        assert!(admin.labels_plural.is_some());
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

        let admin = parse_field_admin(&lua, &admin_tbl).unwrap();
        assert_eq!(admin.languages, vec!["javascript", "python", "html"]);
    }

    #[test]
    fn test_parse_field_admin_languages_default_empty() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let admin = parse_field_admin(&lua, &admin_tbl).unwrap();
        assert!(admin.languages.is_empty());
    }

    #[test]
    fn test_parse_field_admin_resizable_false() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("resizable", false).unwrap();
        let admin = parse_field_admin(&lua, &admin_tbl).unwrap();
        assert!(!admin.resizable);
    }

    #[test]
    fn test_parse_field_admin_template_safe_path_accepted() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("template", "fields/rating").unwrap();
        let admin = parse_field_admin(&lua, &admin_tbl).unwrap();
        assert_eq!(admin.template.as_deref(), Some("fields/rating"));
    }

    #[test]
    fn test_parse_field_admin_template_unsafe_path_rejected() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("template", "../../etc/passwd").unwrap();
        let err = parse_field_admin(&lua, &admin_tbl).unwrap_err();
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

        let admin = parse_field_admin(&lua, &admin_tbl).unwrap();
        assert_eq!(
            admin.extra.get("icon").and_then(|v| v.as_str()),
            Some("star")
        );
        assert_eq!(
            admin.extra.get("max_stars").and_then(|v| v.as_i64()),
            Some(5)
        );
        assert_eq!(
            admin.extra.get("rounded").and_then(|v| v.as_bool()),
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

        let err = parse_field_admin(&lua, &admin_tbl).unwrap_err();
        assert!(
            err.to_string().contains("must be a table"),
            "expected sequence-rejection error, got: {err}"
        );
    }

    #[test]
    fn test_parse_field_admin_extra_default_empty() {
        let lua = Lua::new();
        let admin_tbl = lua.create_table().unwrap();
        let admin = parse_field_admin(&lua, &admin_tbl).unwrap();
        assert!(admin.extra.is_empty());
    }
}
