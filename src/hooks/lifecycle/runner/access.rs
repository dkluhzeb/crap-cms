//! HookRunner methods for auth strategies and access control.

use std::collections::HashMap;

use anyhow::Result;
use mlua::Value;
use serde_json::Value as JsonValue;
use tracing::error;

use crate::{
    core::{Document, FieldDefinition, FieldType, document::DocumentBuilder},
    db::{AccessResult, DbConnection, query::helpers::prefixed_name},
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

/// Convert a Lua table returned by an auth strategy into a Document.
fn lua_table_to_auth_user(lua: &mlua::Lua, tbl: &mlua::Table) -> Result<Document> {
    let id: String = tbl.get("id")?;
    let mut fields = HashMap::new();

    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;

        if k == "id" || k == "created_at" || k == "updated_at" {
            continue;
        }

        fields.insert(k, api::lua_to_json(lua, &v)?);
    }

    let created_at: Option<String> = tbl.get("created_at").ok();
    let updated_at: Option<String> = tbl.get("updated_at").ok();

    Ok(DocumentBuilder::new(id)
        .fields(fields)
        .created_at(created_at)
        .updated_at(updated_at)
        .build())
}

impl HookRunner {
    /// Run a custom auth strategy function. Takes a strategy function ref and
    /// a headers map, returns Some(Document) if the strategy authenticates a user.
    /// The strategy function gets CRUD access via the provided connection.
    pub fn run_auth_strategy(
        &self,
        authenticate_ref: &str,
        collection: &str,
        headers: &HashMap<String, String>,
        conn: &dyn DbConnection,
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
            Value::Table(tbl) => Ok(Some(lua_table_to_auth_user(&lua, &tbl)?)),
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
        conn: &dyn DbConnection,
    ) -> Result<AccessResult> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, None, None);

        check_access_with_lua(&lua, access_ref, user, id, data)
    }

    /// Check field-level read access. Returns a list of field names that should be
    /// stripped from the response (denied fields).
    ///
    /// Fail-closed: if the Lua VM pool is exhausted, all access-controlled fields
    /// are denied rather than silently allowed.
    pub fn check_field_read_access(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        conn: &dyn DbConnection,
    ) -> Vec<String> {
        // Skip VM acquisition if no fields have read access functions (recursive check)
        if !has_any_field_access(fields, |f| f.access.read.as_deref()) {
            return Vec::new();
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                error!("Lua VM pool exhausted during field read access check: {e}");

                return deny_all_access_controlled(fields, |f| f.access.read.as_deref());
            }
        };

        let _guard = TxContextGuard::set(&lua, conn, None, None);

        check_field_read_access_with_lua(&lua, fields, user)
    }

    /// Check field-level write access for a given operation ("create" or "update").
    /// Returns a list of field names that should be stripped from the input.
    ///
    /// Fail-closed: if the Lua VM pool is exhausted, all access-controlled fields
    /// are denied rather than silently allowed.
    pub fn check_field_write_access(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        operation: &str,
        conn: &dyn DbConnection,
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
            Err(e) => {
                error!("Lua VM pool exhausted during field write access check: {e}");

                return deny_all_access_controlled(fields, extractor);
            }
        };

        let _guard = TxContextGuard::set(&lua, conn, None, None);

        check_field_write_access_with_lua(&lua, fields, user, operation)
    }
}

/// Collect names of all fields that have an access control function configured.
/// Used as fail-closed fallback when the Lua VM pool is unavailable.
/// Recurses into Group (with `__` prefix), Row/Collapsible/Tabs (transparent).
fn deny_all_access_controlled(
    fields: &[FieldDefinition],
    extractor: impl Fn(&FieldDefinition) -> Option<&str> + Copy,
) -> Vec<String> {
    deny_all_recursive(fields, &extractor, "")
}

fn deny_all_recursive(
    fields: &[FieldDefinition],
    extractor: &(impl Fn(&FieldDefinition) -> Option<&str> + Copy),
    prefix: &str,
) -> Vec<String> {
    let mut denied = Vec::new();

    for field in fields {
        let full_name = prefixed_name(prefix, &field.name);

        if extractor(field).is_some() {
            denied.push(full_name.clone());
        }

        match field.field_type {
            FieldType::Group => {
                denied.extend(deny_all_recursive(&field.fields, extractor, &full_name));
            }
            FieldType::Row | FieldType::Collapsible => {
                denied.extend(deny_all_recursive(&field.fields, extractor, prefix));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    denied.extend(deny_all_recursive(&tab.fields, extractor, prefix));
                }
            }
            _ => {}
        }
    }

    denied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldAccess, FieldTab};

    fn make_field(name: &str, access: FieldAccess) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(access)
            .build()
    }

    #[test]
    fn deny_all_finds_top_level() {
        let fields = vec![make_field(
            "secret",
            FieldAccess {
                read: Some("hooks.deny".to_string()),
                ..Default::default()
            },
        )];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_deref());
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn deny_all_recurses_into_group_with_prefix() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "title",
                    FieldAccess {
                        read: Some("hooks.deny".to_string()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_deref());
        assert_eq!(denied, vec!["seo__title"]);
    }

    #[test]
    fn deny_all_recurses_into_tabs() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Main",
                    vec![make_field(
                        "hidden",
                        FieldAccess {
                            read: Some("hooks.deny".to_string()),
                            ..Default::default()
                        },
                    )],
                )])
                .build(),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_deref());
        assert_eq!(denied, vec!["hidden"]);
    }

    #[test]
    fn deny_all_empty_when_no_access_configured() {
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_deref());
        assert!(denied.is_empty());
    }
}
