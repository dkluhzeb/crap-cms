//! HookRunner methods for auth strategies and access control.

use std::collections::HashMap;

use anyhow::Result;
use mlua::Value;
use serde_json::Value as JsonValue;

use crate::{
    core::{Document, FieldDefinition, document::DocumentBuilder},
    db::AccessResult,
    hooks::{
        HookRunner, api,
        lifecycle::{
            access::{
                check_access_with_lua, check_field_read_access_with_lua,
                check_field_write_access_with_lua, has_any_field_access,
            },
            execution::resolve_hook_function,
            types::TxContextGuard,
        },
    },
};

impl HookRunner {
    /// Run a custom auth strategy function. Takes a strategy function ref and
    /// a headers map, returns Some(Document) if the strategy authenticates a user.
    /// The strategy function gets CRUD access via the provided connection.
    pub fn run_auth_strategy(
        &self,
        authenticate_ref: &str,
        collection: &str,
        headers: &HashMap<String, String>,
        conn: &dyn crate::db::DbConnection,
    ) -> Result<Option<Document>> {
        let lua = self.pool.acquire()?;

        // Inject connection for CRUD access — guard ensures cleanup on all exit paths
        let _guard = TxContextGuard::set(&lua, conn, None, None);

        let func = resolve_hook_function(&lua, authenticate_ref)?;

        // Build context table: { headers = {...}, collection = "..." }
        let ctx_table = lua.create_table()?;
        let headers_table = lua.create_table()?;
        for (k, v) in headers {
            headers_table.set(k.as_str(), v.as_str())?;
        }
        ctx_table.set("headers", headers_table)?;
        ctx_table.set("collection", collection)?;

        let result: Value = func.call(ctx_table)?;

        match result {
            Value::Table(tbl) => {
                // Strategy returned a user table — convert to Document
                let id: String = tbl.get("id")?;
                let mut fields = HashMap::new();
                for pair in tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;

                    if k == "id" || k == "created_at" || k == "updated_at" {
                        continue;
                    }
                    fields.insert(k, api::lua_to_json(&lua, &v)?);
                }
                let created_at: Option<String> = tbl.get("created_at").ok();
                let updated_at: Option<String> = tbl.get("updated_at").ok();
                Ok(Some(
                    DocumentBuilder::new(id)
                        .fields(fields)
                        .created_at(created_at)
                        .updated_at(updated_at)
                        .build(),
                ))
            }
            Value::Nil | Value::Boolean(false) => Ok(None),
            _ => Ok(None),
        }
    }

    /// Run a collection-level or global-level access check.
    ///
    /// `access_ref` is the Lua function ref (e.g., "hooks.access.admin_only").
    /// If `None`, access is allowed (no restriction configured).
    /// The function receives `{ user = ..., id = ..., data = ... }` and returns:
    /// - `true` → Allowed
    /// - `false` / `nil` → Denied
    /// - `table` → Constrained (read only: additional WHERE filters)
    pub fn check_access(
        &self,
        access_ref: Option<&str>,
        user: Option<&Document>,
        id: Option<&str>,
        data: Option<&HashMap<String, JsonValue>>,
        conn: &dyn crate::db::DbConnection,
    ) -> Result<AccessResult> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, None, None);
        check_access_with_lua(&lua, access_ref, user, id, data)
    }

    /// Check field-level read access. Returns a list of field names that should be
    /// stripped from the response (denied fields).
    pub fn check_field_read_access(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        conn: &dyn crate::db::DbConnection,
    ) -> Vec<String> {
        // Skip VM acquisition if no fields have read access functions (recursive check)
        if !has_any_field_access(fields, |f| f.access.read.as_deref()) {
            return Vec::new();
        }
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };
        let _guard = TxContextGuard::set(&lua, conn, None, None);
        check_field_read_access_with_lua(&lua, fields, user)
    }

    /// Check field-level write access for a given operation ("create" or "update").
    /// Returns a list of field names that should be stripped from the input.
    pub fn check_field_write_access(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        operation: &str,
        conn: &dyn crate::db::DbConnection,
    ) -> Vec<String> {
        // Skip VM acquisition if no fields have write access functions (recursive check)
        let extractor: fn(&FieldDefinition) -> Option<&str> = match operation {
            "create" => |f| f.access.create.as_deref(),
            "update" => |f| f.access.update.as_deref(),
            _ => return Vec::new(),
        };
        if !has_any_field_access(fields, extractor) {
            return Vec::new();
        }
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };
        let _guard = TxContextGuard::set(&lua, conn, None, None);
        check_field_write_access_with_lua(&lua, fields, user, operation)
    }
}
