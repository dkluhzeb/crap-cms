//! Register `crap.access` — collection and field-level access checks.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::{
    core::{Document, SharedRegistry},
    db::AccessResult,
    hooks::lifecycle::{
        UserContext,
        access::{
            check_access_with_lua, check_field_read_access_with_lua,
            check_field_write_access_with_lua,
        },
    },
};

/// Register `crap.access.check`, `crap.access.field_read_denied`, and
/// `crap.access.field_write_denied` Lua functions.
pub(super) fn register_access(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let access_table = lua.create_table()?;

    let reg = registry.clone();
    access_table.set(
        "check",
        lua.create_function(move |lua, (collection, operation): (String, String)| {
            check(lua, &reg, &collection, &operation)
        })?,
    )?;

    let reg = registry.clone();
    access_table.set(
        "field_read_denied",
        lua.create_function(move |lua, collection: String| {
            field_read_denied(lua, &reg, &collection)
        })?,
    )?;

    let reg = registry;
    access_table.set(
        "field_write_denied",
        lua.create_function(move |lua, (collection, operation): (String, String)| {
            field_write_denied(lua, &reg, &collection, &operation)
        })?,
    )?;

    crap.set("access", access_table)?;

    Ok(())
}

/// Extract the current user from Lua app_data.
fn current_user(lua: &Lua) -> Option<Document> {
    lua.app_data_ref::<UserContext>()
        .and_then(|ctx| ctx.0.clone())
}

/// Look up the access function ref for a given operation on a collection.
fn resolve_access_ref(
    registry: &SharedRegistry,
    collection: &str,
    operation: &str,
) -> LuaResult<Option<String>> {
    let r = registry
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    // Try as collection first, then as global.
    if let Some(def) = r.get_collection(collection) {
        let access_ref = match operation {
            "read" => def.access.read.clone(),
            "create" => def.access.create.clone(),
            "update" => def.access.update.clone(),
            "delete" => def.access.delete.clone(),
            "trash" => def
                .access
                .trash
                .clone()
                .or_else(|| def.access.update.clone()),
            _ => return Err(RuntimeError(format!("Unknown operation '{operation}'"))),
        };

        return Ok(access_ref);
    }

    if let Some(def) = r.get_global(collection) {
        let access_ref = match operation {
            "read" => def.access.read.clone(),
            "update" => def.access.update.clone(),
            _ => {
                return Err(RuntimeError(format!(
                    "Operation '{operation}' not valid for global '{collection}'"
                )));
            }
        };

        return Ok(access_ref);
    }

    Err(RuntimeError(format!(
        "Collection or global '{collection}' not found"
    )))
}

/// `crap.access.check(collection, operation)` -> `"allowed"` | `"denied"` | table
///
/// Evaluates the configured access function for the given collection and operation
/// against the current user. Returns `"allowed"`, `"denied"`, or a table of
/// constraint filters.
fn check(
    lua: &Lua,
    registry: &SharedRegistry,
    collection: &str,
    operation: &str,
) -> LuaResult<Value> {
    let access_ref = resolve_access_ref(registry, collection, operation)?;
    let user = current_user(lua);

    let result = check_access_with_lua(lua, access_ref.as_deref(), user.as_ref(), None, None)
        .map_err(|e| RuntimeError(format!("access check error: {e:#}")))?;

    match result {
        AccessResult::Allowed => Ok(Value::String(lua.create_string("allowed")?)),
        AccessResult::Denied => Ok(Value::String(lua.create_string("denied")?)),
        AccessResult::Constrained(clauses) => {
            let tbl = constraints_to_lua(lua, &clauses)?;
            Ok(Value::Table(tbl))
        }
    }
}

/// Convert constraint filter clauses to a Lua table for return to user code.
fn constraints_to_lua(lua: &Lua, clauses: &[crate::db::FilterClause]) -> LuaResult<Table> {
    use crate::db::FilterClause;

    let tbl = lua.create_table()?;

    for clause in clauses {
        match clause {
            FilterClause::Single(filter) => {
                let value = filter_op_to_lua(lua, &filter.op)?;
                tbl.set(filter.field.as_str(), value)?;
            }
            FilterClause::Or(groups) => {
                let or_tbl = lua.create_table()?;

                for (i, group) in groups.iter().enumerate() {
                    let group_tbl = lua.create_table()?;

                    for filter in group {
                        let value = filter_op_to_lua(lua, &filter.op)?;
                        group_tbl.set(filter.field.as_str(), value)?;
                    }

                    or_tbl.set(i + 1, group_tbl)?;
                }

                tbl.set("_or", or_tbl)?;
            }
        }
    }

    Ok(tbl)
}

/// Convert a single `FilterOp` to a Lua value.
fn filter_op_to_lua(lua: &Lua, op: &crate::db::FilterOp) -> LuaResult<Value> {
    use crate::db::FilterOp;

    match op {
        FilterOp::Equals(v) => Ok(Value::String(lua.create_string(v)?)),
        _ => {
            // For complex operators, return a table { op_name = value }
            let op_tbl = lua.create_table()?;
            let (name, val) = filter_op_name_value(op);

            match val {
                OpValue::Single(s) => op_tbl.set(name, lua.create_string(&s)?)?,
                OpValue::List(items) => {
                    let arr = lua.create_table()?;
                    for (i, s) in items.iter().enumerate() {
                        arr.set(i + 1, lua.create_string(s)?)?;
                    }
                    op_tbl.set(name, arr)?;
                }
                OpValue::None => op_tbl.set(name, true)?,
            }

            Ok(Value::Table(op_tbl))
        }
    }
}

enum OpValue {
    Single(String),
    List(Vec<String>),
    None,
}

/// Extract operator name and value for Lua table representation.
fn filter_op_name_value(op: &crate::db::FilterOp) -> (&'static str, OpValue) {
    use crate::db::FilterOp;

    match op {
        FilterOp::Equals(v) => ("equals", OpValue::Single(v.clone())),
        FilterOp::NotEquals(v) => ("not_equals", OpValue::Single(v.clone())),
        FilterOp::Like(v) => ("like", OpValue::Single(v.clone())),
        FilterOp::Contains(v) => ("contains", OpValue::Single(v.clone())),
        FilterOp::GreaterThan(v) => ("greater_than", OpValue::Single(v.clone())),
        FilterOp::LessThan(v) => ("less_than", OpValue::Single(v.clone())),
        FilterOp::GreaterThanOrEqual(v) => ("greater_than_equal", OpValue::Single(v.clone())),
        FilterOp::LessThanOrEqual(v) => ("less_than_equal", OpValue::Single(v.clone())),
        FilterOp::In(v) => ("in", OpValue::List(v.clone())),
        FilterOp::NotIn(v) => ("not_in", OpValue::List(v.clone())),
        FilterOp::Exists => ("exists", OpValue::None),
        FilterOp::NotExists => ("not_exists", OpValue::None),
    }
}

/// `crap.access.field_read_denied(collection)` -> `{string}`
///
/// Returns an array of field names the current user cannot read.
fn field_read_denied(
    lua: &Lua,
    registry: &SharedRegistry,
    collection: &str,
) -> LuaResult<Vec<String>> {
    let fields = resolve_fields(registry, collection)?;
    let user = current_user(lua);

    Ok(check_field_read_access_with_lua(
        lua,
        &fields,
        user.as_ref(),
    ))
}

/// `crap.access.field_write_denied(collection, operation)` -> `{string}`
///
/// Returns an array of field names the current user cannot write.
/// `operation` must be `"create"` or `"update"`.
fn field_write_denied(
    lua: &Lua,
    registry: &SharedRegistry,
    collection: &str,
    operation: &str,
) -> LuaResult<Vec<String>> {
    if operation != "create" && operation != "update" {
        return Err(RuntimeError(format!(
            "field_write_denied operation must be 'create' or 'update', got '{operation}'"
        )));
    }

    let fields = resolve_fields(registry, collection)?;
    let user = current_user(lua);

    Ok(check_field_write_access_with_lua(
        lua,
        &fields,
        user.as_ref(),
        operation,
    ))
}

/// Look up the field definitions for a collection or global.
fn resolve_fields(
    registry: &SharedRegistry,
    collection: &str,
) -> LuaResult<Vec<crate::core::FieldDefinition>> {
    let r = registry
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    if let Some(def) = r.get_collection(collection) {
        return Ok(def.fields.clone());
    }

    if let Some(def) = r.get_global(collection) {
        return Ok(def.fields.clone());
    }

    Err(RuntimeError(format!(
        "Collection or global '{collection}' not found"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CollectionDefinition, Registry, SharedRegistry,
        collection::{Access, GlobalDefinition},
    };
    use crate::db::{Filter, FilterClause, FilterOp};
    use mlua::Lua;
    use std::sync::{Arc, RwLock};

    fn make_registry() -> SharedRegistry {
        Arc::new(RwLock::new(Registry::new()))
    }

    fn make_registry_with_collection(slug: &str, access: Access) -> SharedRegistry {
        let def = CollectionDefinition::builder(slug).access(access).build();
        let registry = make_registry();
        registry.write().unwrap().register_collection(def);
        registry
    }

    #[test]
    fn check_returns_allowed_when_no_access_configured() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let result = check(&lua, &registry, "posts", "read").unwrap();

        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "allowed"),
            other => panic!("Expected string 'allowed', got {:?}", other),
        }
    }

    #[test]
    fn check_returns_error_for_unknown_collection() {
        let lua = Lua::new();
        let registry = make_registry();

        let result = check(&lua, &registry, "nonexistent", "read");
        assert!(result.is_err());
    }

    #[test]
    fn check_returns_error_for_unknown_operation() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let result = check(&lua, &registry, "posts", "invalid_op");
        assert!(result.is_err());
    }

    #[test]
    fn field_read_denied_returns_empty_when_no_access() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let denied = field_read_denied(&lua, &registry, "posts").unwrap();
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_denied_rejects_invalid_operation() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let result = field_write_denied(&lua, &registry, "posts", "delete");
        assert!(result.is_err());
    }

    #[test]
    fn constraints_to_lua_converts_single_filter() {
        let lua = Lua::new();
        let clauses = vec![FilterClause::Single(Filter {
            field: "author".to_string(),
            op: FilterOp::Equals("user_1".to_string()),
        })];

        let tbl = constraints_to_lua(&lua, &clauses).unwrap();
        let val: String = tbl.get("author").unwrap();
        assert_eq!(val, "user_1");
    }

    #[test]
    fn constraints_to_lua_converts_complex_op() {
        let lua = Lua::new();
        let clauses = vec![FilterClause::Single(Filter {
            field: "age".to_string(),
            op: FilterOp::GreaterThan("18".to_string()),
        })];

        let tbl = constraints_to_lua(&lua, &clauses).unwrap();
        let op_tbl: Table = tbl.get("age").unwrap();
        let val: String = op_tbl.get("greater_than").unwrap();
        assert_eq!(val, "18");
    }

    #[test]
    fn current_user_returns_none_without_context() {
        let lua = Lua::new();
        assert!(current_user(&lua).is_none());
    }

    #[test]
    fn resolve_access_ref_for_global() {
        let registry = make_registry();
        let global = GlobalDefinition::builder("settings")
            .access(
                Access::builder()
                    .read(Some("check_read".to_string()))
                    .build(),
            )
            .build();
        registry.write().unwrap().register_global(global);

        let access = resolve_access_ref(&registry, "settings", "read").unwrap();
        assert_eq!(access.as_deref(), Some("check_read"));
    }

    #[test]
    fn resolve_access_ref_global_rejects_create() {
        let registry = make_registry();
        let global = GlobalDefinition::builder("settings").build();
        registry.write().unwrap().register_global(global);

        let result = resolve_access_ref(&registry, "settings", "create");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_access_ref_trash_falls_back_to_update() {
        let registry = make_registry_with_collection(
            "posts",
            Access::builder()
                .update(Some("check_update".to_string()))
                .build(),
        );

        let access = resolve_access_ref(&registry, "posts", "trash").unwrap();
        assert_eq!(access.as_deref(), Some("check_update"));
    }
}
