//! Parsing functions for field relationship configuration.

use mlua::Table;

use crate::core::field::{FieldType, RelationshipConfig};

use super::helpers::*;

/// Parse the relationship config for a field, handling both `Relationship` and `Upload` field types.
/// Returns `None` for field types that don't have a relationship config.
pub(super) fn parse_field_relationship(
    field_tbl: &Table,
    field_type: &FieldType,
) -> mlua::Result<Option<RelationshipConfig>> {
    if *field_type == FieldType::Relationship {
        if let Ok(rel_tbl) = get_table(field_tbl, "relationship") {
            let (collection, polymorphic) = parse_relationship_collection(&rel_tbl);
            let has_many = get_bool(&rel_tbl, "has_many", false);
            let max_depth = rel_tbl.get::<Option<i32>>("max_depth").ok().flatten();
            let mut rc = RelationshipConfig::new(collection, has_many);
            rc.max_depth = max_depth;
            rc.polymorphic = polymorphic;
            Ok(Some(rc))
        } else {
            // Legacy flat syntax: relation_to + has_many on the field itself
            Ok(get_string(field_tbl, "relation_to").map(|collection| {
                let has_many = get_bool(field_tbl, "has_many", false);
                RelationshipConfig::new(collection, has_many)
            }))
        }
    } else if *field_type == FieldType::Upload {
        // Upload: relationship config from relation_to or relationship table
        if let Ok(rel_tbl) = get_table(field_tbl, "relationship") {
            let collection = get_string(&rel_tbl, "collection").unwrap_or_default();
            let has_many = get_bool(&rel_tbl, "has_many", false);
            let max_depth = rel_tbl.get::<Option<i32>>("max_depth").ok().flatten();
            let mut rc = RelationshipConfig::new(collection, has_many);
            rc.max_depth = max_depth;
            Ok(Some(rc))
        } else {
            let collection = get_string(field_tbl, "relation_to");
            let has_many = get_bool(field_tbl, "has_many", false);
            Ok(collection.map(|collection| RelationshipConfig::new(collection, has_many)))
        }
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fields::parse_fields;
    use mlua::Lua;

    #[test]
    fn test_parse_fields_relationship_table_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "author").unwrap();
        field.set("type", "relationship").unwrap();
        let rel = lua.create_table().unwrap();
        rel.set("collection", "users").unwrap();
        rel.set("has_many", false).unwrap();
        rel.set("max_depth", 2i32).unwrap();
        field.set("relationship", rel).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let rel = fields[0].relationship.as_ref().unwrap();
        assert_eq!(rel.collection, "users");
        assert!(!rel.has_many);
        assert_eq!(rel.max_depth, Some(2));
    }

    #[test]
    fn test_parse_fields_relationship_legacy_flat_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "author").unwrap();
        field.set("type", "relationship").unwrap();
        field.set("relation_to", "users").unwrap();
        field.set("has_many", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let rel = fields[0].relationship.as_ref().unwrap();
        assert_eq!(rel.collection, "users");
        assert!(rel.has_many);
        assert!(rel.max_depth.is_none());
    }

    #[test]
    fn test_parse_fields_relationship_no_relation_to_returns_none() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "ref").unwrap();
        field.set("type", "relationship").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].relationship.is_none());
    }

    #[test]
    fn test_parse_fields_upload_relationship_table_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "avatar").unwrap();
        field.set("type", "upload").unwrap();
        let rel = lua.create_table().unwrap();
        rel.set("collection", "media").unwrap();
        rel.set("has_many", false).unwrap();
        rel.set("max_depth", 1i32).unwrap();
        field.set("relationship", rel).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let r = fields[0].relationship.as_ref().unwrap();
        assert_eq!(r.collection, "media");
        assert_eq!(r.max_depth, Some(1));
    }

    #[test]
    fn test_parse_fields_upload_relationship_flat_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "avatar").unwrap();
        field.set("type", "upload").unwrap();
        field.set("relation_to", "media").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let r = fields[0].relationship.as_ref().unwrap();
        assert_eq!(r.collection, "media");
    }

    #[test]
    fn test_parse_fields_upload_no_relation_to() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "doc").unwrap();
        field.set("type", "upload").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].relationship.is_none());
    }
}
