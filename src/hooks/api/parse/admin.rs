//! Parsing functions for field admin configuration.

use mlua::Table;

use crate::core::field::FieldAdmin;

use super::helpers::*;

/// Parse the `admin` subtable of a field Lua definition into a `FieldAdmin`.
pub(super) fn parse_field_admin(admin_tbl: &Table) -> mlua::Result<FieldAdmin> {
    let (labels_singular, labels_plural) = if let Ok(labels_tbl) = get_table(admin_tbl, "labels") {
        (get_localized_string(&labels_tbl, "singular"), get_localized_string(&labels_tbl, "plural"))
    } else {
        (None, None)
    };
    Ok(FieldAdmin {
        label: get_localized_string(admin_tbl, "label"),
        placeholder: get_localized_string(admin_tbl, "placeholder"),
        description: get_localized_string(admin_tbl, "description"),
        hidden: get_bool(admin_tbl, "hidden", false),
        readonly: get_bool(admin_tbl, "readonly", false),
        width: get_string(admin_tbl, "width"),
        collapsed: get_bool(admin_tbl, "collapsed", true),
        label_field: get_string(admin_tbl, "label_field"),
        row_label: get_string(admin_tbl, "row_label"),
        labels_singular,
        labels_plural,
        position: get_string(admin_tbl, "position"),
        condition: get_string(admin_tbl, "condition"),
        step: get_string(admin_tbl, "step"),
        rows: admin_tbl.get::<Option<u32>>("rows").ok().flatten(),
        language: get_string(admin_tbl, "language"),
        features: if let Ok(tbl) = get_table(admin_tbl, "features") {
            tbl.sequence_values::<String>().filter_map(|r| r.ok()).collect()
        } else {
            Vec::new()
        },
        picker: get_string(admin_tbl, "picker"),
        richtext_format: get_string(admin_tbl, "format"),
        nodes: if let Ok(tbl) = get_table(admin_tbl, "nodes") {
            tbl.sequence_values::<String>().filter_map(|r| r.ok()).collect()
        } else {
            Vec::new()
        },
    })
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
