//! Register `crap.access` — collection and field-level access checks.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::core::{
    Document, DocumentFields, FieldDefinition, FieldDenial, HookRef, Registry, RegistryRead,
    SharedRegistry, nest_group_fields,
};
use crate::db::{AccessResult, FilterClause, FilterOp};
use crate::hooks::lifecycle::{
    AccessCheckInput, UserContext,
    access::{
        check_access_with_lua, check_field_read_access_with_lua, check_field_write_access_with_lua,
        collect_read_denied_with_lua, collect_write_denied_with_lua,
    },
};
use crate::hooks::lua_api::lua_to_json;
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

// ── User-facing fns ──────────────────────────────────────────────────
//
// Six `#[lua_fn]` decls (3 functions × init/pool variants). Each
// variant pulls a `&Registry` from its concrete state via
// `RegistryRead::with` and forwards to one of the `*_impl` helpers
// below. The macro doesn't accept generics or trait bounds in
// `state:`, so the duplication is intentional — init and pool stay
// concrete.

/// Evaluate the access function for a collection/global + operation against the
/// current user. Returns `"allowed"`, `"denied"`, or a constraint-filter table.
#[lua_fn(
    path = "crap.access.check",
    returns = "\"allowed\" | \"denied\" | table",
    returns_doc = "\"allowed\" / \"denied\" string, or a `{ field = filter, ... }` constraint table."
)]
fn access_check_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Collection or global slug.")] collection: String,
    #[lua(
        doc = "Operation: `\"read\"`, `\"create\"`, `\"update\"`, `\"delete\"`, or `\"trash\"`."
    )]
    operation: String,
) -> LuaResult<Value> {
    state
        .with(|r| check_impl(lua, r, &collection, &operation))
        .map_err(|e| RuntimeError(e.to_string()))?
}

#[lua_fn(
    path = "crap.access.check",
    returns = "\"allowed\" | \"denied\" | table"
)]
fn access_check_pool(
    state: &Arc<Registry>,
    lua: &Lua,
    collection: String,
    operation: String,
) -> LuaResult<Value> {
    state
        .with(|r| check_impl(lua, r, &collection, &operation))
        .map_err(|e| RuntimeError(e.to_string()))?
}

/// Return the names of fields the current user cannot read for this
/// collection/global. Pass `document` for a data-aware check (matches the
/// enforcement strip for that row — use it for an edit form); omit it for a
/// categorical, role-only check (`ctx.data` / `ctx.document` nil — use it for a
/// create form). For UI gating only — enforcement is the per-document strip.
#[lua_fn(
    path = "crap.access.field_read_denied",
    returns = "string[]",
    returns_doc = "Names of fields the current user cannot read."
)]
fn access_field_read_denied_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Collection or global slug.")] collection: String,
    #[lua(
        doc = "Optional document to evaluate data-dependent rules against (sets `ctx.data` / `ctx.document`). Omit for a categorical, role-only check."
    )]
    document: Option<Table>,
) -> LuaResult<Vec<String>> {
    let document = parse_optional_document(document)?;
    state
        .with(|r| field_read_denied_impl(lua, r, &collection, document.as_ref()))
        .map_err(|e| RuntimeError(e.to_string()))?
}

#[lua_fn(path = "crap.access.field_read_denied", returns = "string[]")]
fn access_field_read_denied_pool(
    state: &Arc<Registry>,
    lua: &Lua,
    collection: String,
    document: Option<Table>,
) -> LuaResult<Vec<String>> {
    let document = parse_optional_document(document)?;
    state
        .with(|r| field_read_denied_impl(lua, r, &collection, document.as_ref()))
        .map_err(|e| RuntimeError(e.to_string()))?
}

/// Return the names of fields the current user cannot write for this
/// collection/global under the given operation (`"create"` or `"update"`). Pass
/// `document` for a data-aware check (matches the enforcement strip — use it for
/// an edit form, where the row is known); omit it for a categorical, role-only
/// check (use it for a create form). For UI gating only — enforcement is the
/// per-document strip.
#[lua_fn(
    path = "crap.access.field_write_denied",
    returns = "string[]",
    returns_doc = "Names of fields the current user cannot write."
)]
fn access_field_write_denied_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Collection or global slug.")] collection: String,
    #[lua(doc = "Operation: `\"create\"` or `\"update\"`.")] operation: String,
    #[lua(
        doc = "Optional document to evaluate data-dependent rules against (sets `ctx.data` / `ctx.document`). Omit for a categorical, role-only check."
    )]
    document: Option<Table>,
) -> LuaResult<Vec<String>> {
    let document = parse_optional_document(document)?;
    state
        .with(|r| field_write_denied_impl(lua, r, &collection, &operation, document.as_ref()))
        .map_err(|e| RuntimeError(e.to_string()))?
}

#[lua_fn(path = "crap.access.field_write_denied", returns = "string[]")]
fn access_field_write_denied_pool(
    state: &Arc<Registry>,
    lua: &Lua,
    collection: String,
    operation: String,
    document: Option<Table>,
) -> LuaResult<Vec<String>> {
    let document = parse_optional_document(document)?;
    state
        .with(|r| field_write_denied_impl(lua, r, &collection, &operation, document.as_ref()))
        .map_err(|e| RuntimeError(e.to_string()))?
}

// ── lua_table! groupings ─────────────────────────────────────────────

lua_table! {
    name: crap_access_init,
    path: "crap.access",
    state: SharedRegistry,
    header: "Programmatic access-control checks for collections and globals,\nmirroring the same evaluator the CMS uses internally for read /\ncreate / update / delete decisions.",
    fns: [
        access_check_init,
        access_field_read_denied_init,
        access_field_write_denied_init,
    ],
}

lua_table! {
    name: crap_access_pool,
    path: "crap.access",
    state: Arc<Registry>,
    fns: [
        access_check_pool,
        access_field_read_denied_pool,
        access_field_write_denied_pool,
    ],
}

// ── Registration entry points ────────────────────────────────────────

/// Register `crap.access` for init-phase VMs. Same Lua surface as the
/// pool variant; differs only in registry-state type.
pub(super) fn register_access_init(lua: &Lua, registry: SharedRegistry) -> Result<()> {
    register_crap_access_init(lua, registry)?;
    Ok(())
}

/// Register `crap.access` for pool VMs.
pub(super) fn register_access_pool(lua: &Lua, registry: Arc<Registry>) -> Result<()> {
    register_crap_access_pool(lua, registry)?;
    Ok(())
}

// ── Shared business-logic helpers ────────────────────────────────────

/// Extract the current user from Lua `app_data`.
fn current_user(lua: &Lua) -> Option<Document> {
    lua.app_data_ref::<UserContext>()
        .and_then(|ctx| ctx.0.clone())
}

/// Look up the access function ref for a given operation on a collection.
fn resolve_access_ref(
    registry: &Registry,
    collection: &str,
    operation: &str,
) -> LuaResult<Option<HookRef>> {
    // Try as collection first, then as global.
    if let Some(def) = registry.get_collection(collection) {
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
            "unlock" => def
                .access
                .unlock
                .clone()
                .or_else(|| def.access.update.clone()),
            _ => return Err(RuntimeError(format!("Unknown operation '{operation}'"))),
        };

        return Ok(access_ref);
    }

    if let Some(def) = registry.get_global(collection) {
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
fn check_impl(
    lua: &Lua,
    registry: &Registry,
    collection: &str,
    operation: &str,
) -> LuaResult<Value> {
    let access = resolve_access_ref(registry, collection, operation)?;
    let user = current_user(lua);

    let result = check_access_with_lua(
        lua,
        &AccessCheckInput {
            document: None,
            access: access.as_ref(),
            user: user.as_ref(),
            id: None,
            data: None,
            locale: None,
            operation,
            collection,
            ui_locale: None,
        },
    )
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
fn constraints_to_lua(lua: &Lua, clauses: &[FilterClause]) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    for clause in clauses {
        write_clause_to_lua(lua, &tbl, clause)?;
    }

    Ok(tbl)
}

/// Write one [`FilterClause`] tree node into a Lua table. `And` writes its
/// sub-clauses as sibling keys (a table's keys are implicitly AND-ed); `Or`
/// writes an `_or` array of per-alternative tables; recurses through both.
fn write_clause_to_lua(lua: &Lua, tbl: &Table, clause: &FilterClause) -> LuaResult<()> {
    match clause {
        FilterClause::Single(filter) => {
            let value = filter_op_to_lua(lua, &filter.op)?;
            tbl.set(filter.field.as_str(), value)?;
        }
        FilterClause::And(subs) => {
            for c in subs {
                write_clause_to_lua(lua, tbl, c)?;
            }
        }
        FilterClause::Or(subs) => {
            let or_tbl = lua.create_table()?;

            for (i, c) in subs.iter().enumerate() {
                let group_tbl = lua.create_table()?;
                write_clause_to_lua(lua, &group_tbl, c)?;
                or_tbl.set(i + 1, group_tbl)?;
            }

            tbl.set("_or", or_tbl)?;
        }
    }

    Ok(())
}

/// Convert a single `FilterOp` to a Lua value.
fn filter_op_to_lua(lua: &Lua, op: &FilterOp) -> LuaResult<Value> {
    if let FilterOp::Equals(v) = op {
        Ok(Value::String(lua.create_string(v)?))
    } else {
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

enum OpValue {
    Single(String),
    List(Vec<String>),
    None,
}

/// Extract operator name and value for Lua table representation.
fn filter_op_name_value(op: &FilterOp) -> (&'static str, OpValue) {
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

/// `crap.access.field_read_denied(collection [, document])` -> `{string}`
///
/// Returns an array of field names the current user cannot read.
///
/// With a `document` the check is **data-aware**: each `access.read` rule is
/// evaluated with `ctx.data` / `ctx.document` set, so the result matches what the
/// enforcement strip would remove for that row (the right choice for an edit
/// form). Without a `document` it is **categorical** (`ctx.data` / `ctx.document`
/// are nil) — the right choice for a create form or static role-based gating. In
/// neither form is this the enforcement path; enforcement is the per-document
/// strip applied on read.
fn field_read_denied_impl(
    lua: &Lua,
    registry: &Registry,
    collection: &str,
    document: Option<&DocumentFields>,
) -> LuaResult<Vec<String>> {
    let fields = resolve_fields(registry, collection)?;
    let user = current_user(lua);

    let denied = match document {
        Some(doc) => {
            let nested = nest_group_fields(doc, &fields);
            collect_read_denied_with_lua(lua, &fields, &nested, collection, user.as_ref(), None)
        }
        None => check_field_read_access_with_lua(lua, &fields, collection, user.as_ref(), None),
    };

    Ok(denied.iter().map(FieldDenial::display_path).collect())
}

/// `crap.access.field_write_denied(collection, operation [, document])` -> `{string}`
///
/// Returns an array of field names the current user cannot write.
/// `operation` must be `"create"` or `"update"`.
///
/// With a `document` the check is **data-aware** (`ctx.data` / `ctx.document`
/// set), matching what the enforcement strip removes for that payload — the right
/// choice for gating an edit form, where the row being edited is known. Without a
/// `document` it is **categorical** (the right choice for a create form). Not the
/// enforcement path; enforcement is the per-document strip applied on write.
fn field_write_denied_impl(
    lua: &Lua,
    registry: &Registry,
    collection: &str,
    operation: &str,
    document: Option<&DocumentFields>,
) -> LuaResult<Vec<String>> {
    if operation != "create" && operation != "update" {
        return Err(RuntimeError(format!(
            "field_write_denied operation must be 'create' or 'update', got '{operation}'"
        )));
    }

    let fields = resolve_fields(registry, collection)?;
    let user = current_user(lua);

    let denied = match document {
        Some(doc) => {
            let nested = nest_group_fields(doc, &fields);
            collect_write_denied_with_lua(
                lua,
                &fields,
                &nested,
                collection,
                user.as_ref(),
                None,
                operation,
            )
        }
        None => check_field_write_access_with_lua(
            lua,
            &fields,
            collection,
            user.as_ref(),
            None,
            operation,
        ),
    };

    Ok(denied.iter().map(FieldDenial::display_path).collect())
}

/// Convert an optional Lua document table (as passed to the `field_*_denied`
/// introspection helpers) into [`DocumentFields`]. A malformed table is a hard
/// error — the caller explicitly asked for a data-aware check.
fn parse_optional_document(document: Option<Table>) -> LuaResult<Option<DocumentFields>> {
    let Some(table) = document else {
        return Ok(None);
    };

    let json = lua_to_json(&Value::Table(table))?;
    let fields: DocumentFields = serde_json::from_value(json)
        .map_err(|e| RuntimeError(format!("field access document must be a table: {e}")))?;

    Ok(Some(fields))
}

/// Look up the field definitions for a collection or global.
fn resolve_fields(registry: &Registry, collection: &str) -> LuaResult<Vec<FieldDefinition>> {
    if let Some(def) = registry.get_collection(collection) {
        return Ok(def.fields.clone());
    }

    if let Some(def) = registry.get_global(collection) {
        return Ok(def.fields.clone());
    }

    Err(RuntimeError(format!(
        "Collection or global '{collection}' not found"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Access, CollectionDefinition, Registry};
    use crate::db::FilterClause;
    use mlua::Lua;
    use std::sync::Arc;

    fn make_registry() -> Arc<Registry> {
        Arc::new(Registry::new())
    }

    fn make_registry_with_collection(slug: &str, access: Access) -> Arc<Registry> {
        let def = CollectionDefinition::builder(slug).access(access).build();
        let mut reg = Registry::new();
        reg.register_collection(def);
        Arc::new(reg)
    }

    #[test]
    fn check_returns_allowed_when_no_access_configured() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let result = check_impl(&lua, &registry, "posts", "read").unwrap();

        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "allowed"),
            other => panic!("Expected string 'allowed', got {other:?}"),
        }
    }

    #[test]
    fn check_returns_error_for_unknown_collection() {
        let lua = Lua::new();
        let registry = make_registry();

        let result = check_impl(&lua, &registry, "nonexistent", "read");
        assert!(result.is_err());
    }

    #[test]
    fn check_returns_error_for_unknown_operation() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let result = check_impl(&lua, &registry, "posts", "invalid_op");
        assert!(result.is_err());
    }

    #[test]
    fn field_read_denied_returns_empty_when_no_access() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let denied = field_read_denied_impl(&lua, &registry, "posts", None).unwrap();
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_denied_rejects_invalid_operation() {
        let lua = Lua::new();
        let registry = make_registry_with_collection("posts", Access::default());

        let result = field_write_denied_impl(&lua, &registry, "posts", "delete", None);
        assert!(result.is_err());
    }

    #[test]
    fn parse_optional_document_none_is_none() {
        assert!(parse_optional_document(None).unwrap().is_none());
    }

    #[test]
    fn parse_optional_document_converts_table_to_fields() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.set("title", "Hello").unwrap();
        table.set("count", 3).unwrap();

        let fields = parse_optional_document(Some(table)).unwrap().unwrap();
        assert_eq!(
            fields.get("title").and_then(serde_json::Value::as_str),
            Some("Hello")
        );
        assert_eq!(
            fields.get("count").and_then(serde_json::Value::as_i64),
            Some(3)
        );
    }

    // Avoid the unused-import warning when only some of these are
    // exercised — kept for future test additions on filter operators.
    #[allow(dead_code)]
    fn _filter_clause_marker(_c: &FilterClause) {}
}
