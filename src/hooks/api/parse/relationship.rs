//! Parsing functions for field relationship configuration.

use mlua::{Error::RuntimeError, Result as LuaResult, Table};
use tracing::warn;

use crate::core::{FieldType, RelationshipConfig};

use super::helpers::*;

/// Parse the relationship config for a field, handling both `Relationship` and `Upload` field types.
/// Returns `None` for field types that don't have a relationship config.
pub(super) fn parse_field_relationship(
    field_tbl: &Table,
    field_type: &FieldType,
) -> LuaResult<Option<RelationshipConfig>> {
    if !matches!(field_type, FieldType::Relationship | FieldType::Upload) {
        return Ok(None);
    }

    // Try new table syntax first: relationship = { collection = "..." }
    if let Ok(rel_tbl) = get_table(field_tbl, "relationship") {
        return parse_relationship_table(field_tbl, &rel_tbl, field_type);
    }

    // Legacy flat syntax: relation_to = "collection"
    parse_legacy_relation_to(field_tbl)
}

/// Parse the `relationship = { ... }` table syntax.
fn parse_relationship_table(
    field_tbl: &Table,
    rel_tbl: &Table,
    field_type: &FieldType,
) -> LuaResult<Option<RelationshipConfig>> {
    let (collection, polymorphic) = if *field_type == FieldType::Relationship {
        parse_relationship_collection(rel_tbl)
    } else {
        (
            get_string(rel_tbl, "collection").unwrap_or_default(),
            vec![],
        )
    };

    if collection.is_empty() {
        let name = get_string(field_tbl, "name").unwrap_or_default();

        return Err(RuntimeError(format!(
            "Field '{name}': relationship.collection is required"
        )));
    }

    let has_many = get_bool(rel_tbl, "has_many", false)?;
    let max_depth = rel_tbl.get::<Option<i32>>("max_depth").ok().flatten();

    let mut rc = RelationshipConfig::new(collection, has_many);

    rc.max_depth = max_depth;
    rc.polymorphic = polymorphic.into_iter().map(|s| s.into()).collect();

    Ok(Some(rc))
}

/// Parse the deprecated `relation_to = "collection"` flat syntax.
fn parse_legacy_relation_to(field_tbl: &Table) -> LuaResult<Option<RelationshipConfig>> {
    let Some(collection) = get_string(field_tbl, "relation_to") else {
        return Ok(None);
    };

    let field_name = get_string(field_tbl, "name").unwrap_or_default();

    warn!(
        "Field '{field_name}': 'relation_to' is deprecated. \
         Use 'relationship = {{ collection = \"{collection}\" }}' instead."
    );

    let has_many = get_bool(field_tbl, "has_many", false)?;

    Ok(Some(RelationshipConfig::new(collection, has_many)))
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
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
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        let r = fields[0].relationship.as_ref().unwrap();
        assert_eq!(r.collection, "media");
    }

    #[test]
    fn test_parse_relationship_empty_collection_errors() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "ref_field").unwrap();
        field.set("type", "relationship").unwrap();
        let rel = lua.create_table().unwrap();
        rel.set("collection", lua.create_table().unwrap()).unwrap(); // empty array
        field.set("relationship", rel).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(
            err.to_string().contains("collection is required"),
            "Empty collection should error, got: {err}"
        );
    }

    #[test]
    fn test_parse_upload_empty_collection_errors() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "avatar").unwrap();
        field.set("type", "upload").unwrap();
        let rel = lua.create_table().unwrap();
        // collection key missing → defaults to ""
        field.set("relationship", rel).unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err();
        assert!(
            err.to_string().contains("collection is required"),
            "Empty upload collection should error, got: {err}"
        );
    }

    #[test]
    fn test_parse_fields_upload_no_relation_to() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "doc").unwrap();
        field.set("type", "upload").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert!(fields[0].relationship.is_none());
    }
}
