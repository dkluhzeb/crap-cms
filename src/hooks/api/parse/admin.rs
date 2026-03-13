//! Parsing functions for field admin configuration.

use mlua::Table;

use crate::core::field::FieldAdmin;

use super::helpers::*;

/// Parse the `admin` subtable of a field Lua definition into a `FieldAdmin`.
pub(super) fn parse_field_admin(admin_tbl: &Table) -> mlua::Result<FieldAdmin> {
    let (labels_singular, labels_plural) = if let Ok(labels_tbl) = get_table(admin_tbl, "labels") {
        (
            get_localized_string(&labels_tbl, "singular"),
            get_localized_string(&labels_tbl, "plural"),
        )
    } else {
        (None, None)
    };
    let mut builder = FieldAdmin::builder()
        .collapsed(get_bool(admin_tbl, "collapsed", true))
        .hidden(get_bool(admin_tbl, "hidden", false))
        .readonly(get_bool(admin_tbl, "readonly", false));

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
        let admin = parse_field_admin(&admin_tbl).unwrap();
        assert!(admin.labels_singular.is_some());
        assert!(admin.labels_plural.is_some());
        assert_eq!(admin.features, vec!["bold", "italic"]);
        assert_eq!(admin.nodes, vec!["paragraph"]);
        assert_eq!(admin.richtext_format.as_deref(), Some("lexical"));
        assert_eq!(admin.language.as_deref(), Some("en"));
        assert_eq!(admin.rows, Some(5));
    }
}
