//! `HookRunner` methods for auth strategies and access control.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{LuaSerdeExt, Value};
use tracing::error;

use crate::{
    core::{Document, FieldDefinition, FieldType, HookRef, document::DocumentBuilder},
    db::{AccessResult, DbConnection, query::helpers::prefixed_name},
    hooks::{
        HookRunner,
        lifecycle::{
            AccessCheckInput, AuthStrategyContext, AuthStrategyInput,
            access::{
                check_access_with_lua, check_field_read_access_with_lua,
                check_field_write_access_with_lua, has_any_field_access,
            },
            execution::resolve_hook_function,
            types::TxContextGuard,
        },
        lua_api,
    },
};

/// Convert a Lua table returned by an auth strategy into a Document.
fn lua_table_to_auth_user(tbl: &mlua::Table) -> Result<Document> {
    let id: String = tbl.get("id")?;
    let mut fields = HashMap::new();

    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;

        if k == "id" || k == "created_at" || k == "updated_at" {
            continue;
        }

        fields.insert(k, lua_api::lua_to_json(&v)?);
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
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition, function resolution, or the
    /// strategy call itself fails.
    pub fn run_auth_strategy(
        &self,
        authenticate: &HookRef,
        input: &AuthStrategyInput,
        conn: &dyn DbConnection,
    ) -> Result<Option<Document>> {
        let lua = self.pool.acquire()?;

        // Inject connection for CRUD access — guard ensures cleanup on all exit paths
        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        let func = resolve_hook_function(&lua, authenticate.reference())?;

        // Build context table from a typed Rust struct so the Lua-side
        // shape is the single source of truth (see
        // `hooks::lifecycle::AuthStrategyContext`).
        let ctx = AuthStrategyContext {
            headers: input.headers,
            collection: input.collection,
            email: input.email,
            password: input.password,
            remote_addr: input.remote_addr,
            options: authenticate.options(),
        };
        let ctx_value = lua.to_value(&ctx)?;

        let result: Value = func.call(ctx_value)?;

        match result {
            Value::Table(tbl) => Ok(Some(lua_table_to_auth_user(&tbl)?)),
            _ => Ok(None),
        }
    }

    /// Run a collection-level or global-level access check.
    ///
    /// `access_ref` is the Lua function ref (e.g., "`hooks.access.admin_only`").
    /// If `None`, access is allowed (no restriction configured).
    /// The function receives `{ user = ..., id = ..., data = ... }` and returns:
    /// - `true` → Allowed
    /// - `false` / `nil` → Denied
    /// - `table` → Constrained (read only: additional WHERE filters)
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition or the access function call fails.
    pub fn check_access(
        &self,
        input: &AccessCheckInput<'_>,
        conn: &dyn DbConnection,
    ) -> Result<AccessResult> {
        // No access function configured — the in-Lua path would
        // only read the `DefaultDeny` flag from `app_data` and
        // return immediately, so skip the entire VM round-trip.
        // With pool size 16 and 50 concurrent reads, the previous
        // unconditional `pool.acquire()` serialized 34 requests on
        // the VM-pool mutex per tick (was 26% of total CPU spent in
        // futex syscalls). The cached `default_deny` flag is set
        // at builder time from `[access] default_deny`.
        if input.access.is_none() {
            return Ok(if self.default_deny {
                AccessResult::Denied
            } else {
                AccessResult::Allowed
            });
        }

        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        check_access_with_lua(&lua, input)
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
        locale: Option<&str>,
        conn: &dyn DbConnection,
    ) -> Vec<String> {
        // Skip VM acquisition if no fields have read access functions (recursive check)
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return Vec::new();
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                error!("Lua VM pool exhausted during field read access check: {e}");

                return deny_all_access_controlled(fields, |f| f.access.read.as_ref());
            }
        };

        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        check_field_read_access_with_lua(&lua, fields, user, locale)
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
        locale: Option<&str>,
        operation: &str,
        conn: &dyn DbConnection,
    ) -> Vec<String> {
        // Skip VM acquisition if no fields have write access functions (recursive check)
        let extractor: fn(&FieldDefinition) -> Option<&HookRef> = match operation {
            "create" => |f| f.access.create.as_ref(),
            "update" => |f| f.access.update.as_ref(),
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

        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        check_field_write_access_with_lua(&lua, fields, user, locale, operation)
    }
}

/// Collect names of all fields that have an access control function configured.
/// Used as fail-closed fallback when the Lua VM pool is unavailable.
/// Recurses into Group (with `__` prefix), Row/Collapsible/Tabs (transparent).
fn deny_all_access_controlled(
    fields: &[FieldDefinition],
    extractor: impl Fn(&FieldDefinition) -> Option<&HookRef> + Copy,
) -> Vec<String> {
    deny_all_recursive(fields, &extractor, "")
}

fn deny_all_recursive(
    fields: &[FieldDefinition],
    extractor: &(impl Fn(&FieldDefinition) -> Option<&HookRef> + Copy),
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
    use crate::core::{FieldAccess, FieldTab};
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
                read: Some(HookRef::new("hooks.deny")),
                ..Default::default()
            },
        )];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
        assert_eq!(denied, vec!["secret"]);
    }

    #[test]
    fn deny_all_recurses_into_group_with_prefix() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "title",
                    FieldAccess {
                        read: Some(HookRef::new("hooks.deny")),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
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
                            read: Some(HookRef::new("hooks.deny")),
                            ..Default::default()
                        },
                    )],
                )])
                .build(),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
        assert_eq!(denied, vec!["hidden"]);
    }

    #[test]
    fn deny_all_empty_when_no_access_configured() {
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        let denied = deny_all_access_controlled(&fields, |f| f.access.read.as_ref());
        assert!(denied.is_empty());
    }
}
