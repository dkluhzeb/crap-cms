//! Parsing functions for field relationship configuration.

use crate::hooks::lua_api::utils::lua_err;
use mlua::{Error::RuntimeError, Result as LuaResult, Table};
use tracing::warn;

use crate::core::{FieldType, RelationshipConfig};

use super::helpers::{
    deny_unknown_keys, get_bool, get_string, get_table, parse_relationship_collection,
};

/// Parse the relationship config for a field, handling both `Relationship` and `Upload` field types.
/// Returns `None` for field types that don't have a relationship config.
pub(super) fn parse_field_relationship(
    field_tbl: &Table,
    field_type: &FieldType,
) -> LuaResult<Option<RelationshipConfig>> {
    if !field_type.is_reference() {
        return Ok(None);
    }

    // Try new table syntax first: relationship = { collection = "..." }
    if let Ok(rel_tbl) = get_table(field_tbl, "relationship") {
        return parse_relationship_table(field_tbl, &rel_tbl, field_type);
    }

    // Legacy flat syntax: relation_to = "collection"
    if let Some(rc) = parse_legacy_relation_to(field_tbl)? {
        return Ok(Some(rc));
    }

    // No (or wrong-typed) config at all: the field would migrate as a plain
    // TEXT column with no target collection — no populate, no ref-counting,
    // no delete protection. Fail at load instead of silently degrading.
    let name = get_string(field_tbl, "name").unwrap_or_default();
    Err(RuntimeError(format!(
        "Field '{name}': {} fields require relationship = {{ collection = \"...\" }}",
        field_type.as_str()
    )))
}

/// Parse the `relationship = { ... }` table syntax.
fn parse_relationship_table(
    field_tbl: &Table,
    rel_tbl: &Table,
    field_type: &FieldType,
) -> LuaResult<Option<RelationshipConfig>> {
    deny_unknown_keys(
        rel_tbl,
        "relationship",
        &["collection", "has_many", "max_depth"],
    )
    .map_err(lua_err)?;

    let name = get_string(field_tbl, "name").unwrap_or_default();

    let (collection, polymorphic) = if *field_type == FieldType::Relationship {
        parse_relationship_collection(rel_tbl, &name)?
    } else {
        (
            get_string(rel_tbl, "collection").unwrap_or_default(),
            vec![],
        )
    };

    if collection.is_empty() {
        return Err(RuntimeError(format!(
            "Field '{name}': relationship.collection is required"
        )));
    }

    let has_many = get_bool(rel_tbl, "has_many", false)?;

    // A wrong-typed `max_depth` used to be silently dropped (`.ok()`), making
    // the configured depth limit vanish. Hard-error instead.
    let max_depth = rel_tbl.get::<Option<i32>>("max_depth").map_err(|e| {
        RuntimeError(format!(
            "Field '{name}': relationship.max_depth must be an integer: {e}"
        ))
    })?;
    if max_depth.is_some_and(|d| d < 0) {
        return Err(RuntimeError(format!(
            "Field '{name}': relationship.max_depth must not be negative"
        )));
    }

    let mut rc = RelationshipConfig::new(collection, has_many);

    rc.max_depth = max_depth;
    rc.polymorphic = polymorphic
        .into_iter()
        .map(std::convert::Into::into)
        .collect();

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
    use crate::hooks::lua_api::parse::fields::parse_fields;
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

    /// Flipped from `..._returns_none`: a config-less relationship used to
    /// parse (and migrate as a dumb TEXT column); it is now a load error.
    #[test]
    fn test_parse_fields_relationship_no_config_rejected() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "ref").unwrap();
        field.set("type", "relationship").unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err().to_string();
        assert!(err.contains("require relationship"), "{err}");
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

    /// Flipped from accepting a config-less upload: now a load error.
    #[test]
    fn test_parse_fields_upload_no_config_rejected() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "doc").unwrap();
        field.set("type", "upload").unwrap();
        fields_tbl.set(1, field).unwrap();
        let err = parse_fields(&lua, &fields_tbl).unwrap_err().to_string();
        assert!(err.contains("require relationship"), "{err}");
    }

    /// Build one field table, run the full parse, return the Result.
    fn parse_one(
        lua: &Lua,
        set: impl FnOnce(&mlua::Table),
    ) -> anyhow::Result<Vec<crate::core::FieldDefinition>> {
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "rel").unwrap();
        set(&field);
        fields_tbl.set(1, field).unwrap();
        parse_fields(lua, &fields_tbl)
    }

    /// Regression: a relationship/upload field with NO config parsed fine and
    /// migrated as a plain TEXT column — no populate, no ref-counting, no
    /// delete protection. Now a load-time error.
    #[test]
    fn test_relationship_without_config_rejected() {
        let lua = Lua::new();
        let err = parse_one(&lua, |f| {
            f.set("type", "relationship").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("require relationship"), "{err}");

        let err = parse_one(&lua, |f| {
            f.set("type", "upload").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("require relationship"), "{err}");
    }

    /// Regression: a wrong-typed `max_depth` was silently dropped (`.ok()`),
    /// erasing the configured depth limit.
    #[test]
    fn test_relationship_bad_max_depth_rejected() {
        let lua = Lua::new();
        let err = parse_one(&lua, |f| {
            f.set("type", "relationship").unwrap();
            let rel = lua.create_table().unwrap();
            rel.set("collection", "users").unwrap();
            rel.set("max_depth", "deep").unwrap();
            f.set("relationship", rel).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("max_depth"), "{err}");

        let err = parse_one(&lua, |f| {
            f.set("type", "relationship").unwrap();
            let rel = lua.create_table().unwrap();
            rel.set("collection", "users").unwrap();
            rel.set("max_depth", -1i32).unwrap();
            f.set("relationship", rel).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("negative"), "{err}");
    }

    /// Regression: a non-string entry in a polymorphic `collection` array was
    /// silently SKIPPED, shrinking the target set.
    #[test]
    fn test_polymorphic_non_string_entry_rejected() {
        let lua = Lua::new();
        let err = parse_one(&lua, |f| {
            f.set("type", "relationship").unwrap();
            let rel = lua.create_table().unwrap();
            let arr = lua.create_table().unwrap();
            arr.set(1, "posts").unwrap();
            arr.set(2, 42i64).unwrap();
            rel.set("collection", arr).unwrap();
            f.set("relationship", rel).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("entries must be strings"), "{err}");
    }
}
