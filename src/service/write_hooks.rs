//! `WriteHooks` trait and implementations for abstracting write hook execution
//! across different API surfaces (pool-based vs inline Lua VM).

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{Document, FieldDefinition, FieldType, Registry, collection::Hooks},
    db::{DbConnection, query::helpers::prefixed_name},
    hooks::{
        HookContext, HookEvent, HookRunner, ValidationCtx,
        lifecycle::{
            FieldHookEvent,
            access::{check_field_read_access_with_lua, check_field_write_access_with_lua},
            run_before_validate_on_node_attrs, run_field_hooks_inner, run_hooks_inner,
            validate_fields_inner,
        },
    },
};

/// Trait for executing write hooks, abstracting over VM acquisition strategy.
///
/// Two implementations exist:
/// - [`RunnerWriteHooks`]: acquires a Lua VM from the pool (admin, gRPC, MCP)
/// - [`LuaWriteHooks`]: uses the current Lua VM inline (Lua CRUD hooks)
pub trait WriteHooks {
    /// Full before-write pipeline: field BeforeValidate → richtext attr hooks →
    /// collection BeforeValidate → validate → field BeforeChange → collection BeforeChange.
    fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext>;

    /// After-write hooks: field AfterChange → collection AfterChange → registered hooks.
    fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext>;

    /// Run collection-level hooks with CRUD access (for BeforeDelete / AfterDelete).
    fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext>;

    /// Field-level read access: returns denied field names to strip from returned documents.
    fn field_read_denied(&self, fields: &[FieldDefinition], user: Option<&Document>)
    -> Vec<String>;

    /// Collection-level access check. Returns the access result (Allowed/Denied/Constrained).
    fn check_access(
        &self,
        access_ref: Option<&str>,
        user: Option<&Document>,
        id: Option<&str>,
        data: Option<&std::collections::HashMap<String, serde_json::Value>>,
    ) -> Result<crate::db::AccessResult>;

    /// Field-level write access: returns denied field names to strip before persistence.
    fn field_write_denied(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        operation: &str,
    ) -> Vec<String>;
}

/// Pool-based write hook execution for admin, gRPC, and MCP surfaces.
pub struct RunnerWriteHooks<'a> {
    pub runner: &'a HookRunner,
    /// Whether hooks are enabled. When `false`, hook calls are skipped (but validation
    /// still runs in `run_before_write`). Defaults to `true` when not set.
    pub hooks_enabled: bool,
    /// Optional connection for field-level write access checks. When provided,
    /// `field_write_denied` actually checks access via Lua. When `None`, returns empty.
    pub conn: Option<&'a dyn DbConnection>,
}

impl<'a> RunnerWriteHooks<'a> {
    /// Create with hooks enabled and no field access connection (the common case).
    pub fn new(runner: &'a HookRunner) -> Self {
        Self {
            runner,
            hooks_enabled: true,
            conn: None,
        }
    }
}

impl WriteHooks for RunnerWriteHooks<'_> {
    fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext> {
        if self.hooks_enabled {
            self.runner.run_before_write(hooks, fields, ctx, val_ctx)
        } else {
            // Still validate, but skip hooks
            self.runner.validate_fields(fields, &ctx.data, val_ctx)?;
            Ok(ctx)
        }
    }

    fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        if self.hooks_enabled {
            self.runner.run_after_write(hooks, fields, event, ctx, conn)
        } else {
            Ok(ctx)
        }
    }

    fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        if self.hooks_enabled {
            self.runner.run_hooks_with_conn(hooks, event, ctx, conn)
        } else {
            Ok(ctx)
        }
    }

    fn field_read_denied(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
    ) -> Vec<String> {
        let Some(conn) = self.conn else {
            return Vec::new();
        };
        self.runner.check_field_read_access(fields, user, conn)
    }

    fn check_access(
        &self,
        access_ref: Option<&str>,
        user: Option<&Document>,
        id: Option<&str>,
        data: Option<&std::collections::HashMap<String, serde_json::Value>>,
    ) -> Result<crate::db::AccessResult> {
        let Some(conn) = self.conn else {
            return Ok(crate::db::AccessResult::Allowed);
        };
        self.runner.check_access(access_ref, user, id, data, conn)
    }

    fn field_write_denied(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        operation: &str,
    ) -> Vec<String> {
        let Some(conn) = self.conn else {
            return Vec::new();
        };
        self.runner
            .check_field_write_access(fields, user, operation, conn)
    }
}

/// Inline Lua VM write hook execution for Lua CRUD hooks.
pub struct LuaWriteHooks<'a> {
    pub lua: &'a mlua::Lua,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
    pub override_access: bool,
    pub registry: Option<&'a Registry>,
    /// Whether hooks are enabled (false when hook depth exceeded or `hooks: false` option).
    pub hooks_enabled: bool,
    /// Whether validation should run (`hooks` option from Lua API).
    pub run_validation: bool,
}

impl WriteHooks for LuaWriteHooks<'_> {
    fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        mut ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext> {
        if self.hooks_enabled {
            run_field_hooks_inner(
                self.lua,
                fields,
                &FieldHookEvent::BeforeValidate,
                &mut ctx.data,
                &ctx.collection,
                &ctx.operation,
            )?;

            if let Some(registry) = self.registry {
                apply_richtext_before_validate(
                    self.lua,
                    fields,
                    &mut ctx.data,
                    registry,
                    &ctx.collection,
                );
            }

            ctx = run_hooks_inner(self.lua, hooks, HookEvent::BeforeValidate, ctx)?;
        }

        if self.run_validation {
            validate_fields_inner(self.lua, fields, &ctx.data, val_ctx)?;
        }

        if self.hooks_enabled {
            run_field_hooks_inner(
                self.lua,
                fields,
                &FieldHookEvent::BeforeChange,
                &mut ctx.data,
                &ctx.collection,
                &ctx.operation,
            )?;

            ctx = run_hooks_inner(self.lua, hooks, HookEvent::BeforeChange, ctx)?;
        }

        Ok(ctx)
    }

    fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        mut ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        if !self.hooks_enabled {
            return Ok(ctx);
        }

        if matches!(event, HookEvent::AfterChange) {
            run_field_hooks_inner(
                self.lua,
                fields,
                &FieldHookEvent::AfterChange,
                &mut ctx.data,
                &ctx.collection,
                &ctx.operation,
            )?;
        }

        run_hooks_inner(self.lua, hooks, event, ctx)
    }

    fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        if !self.hooks_enabled {
            return Ok(ctx);
        }
        run_hooks_inner(self.lua, hooks, event, ctx)
    }

    fn check_access(
        &self,
        access_ref: Option<&str>,
        user: Option<&Document>,
        id: Option<&str>,
        data: Option<&std::collections::HashMap<String, serde_json::Value>>,
    ) -> Result<crate::db::AccessResult> {
        if self.override_access {
            return Ok(crate::db::AccessResult::Allowed);
        }
        crate::hooks::lifecycle::access::check_access_with_lua(self.lua, access_ref, user, id, data)
    }

    fn field_read_denied(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
    ) -> Vec<String> {
        if self.override_access {
            return Vec::new();
        }
        check_field_read_access_with_lua(self.lua, fields, user)
    }

    fn field_write_denied(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        operation: &str,
    ) -> Vec<String> {
        if self.override_access {
            return Vec::new();
        }
        check_field_write_access_with_lua(self.lua, fields, user, operation)
    }
}

/// Run richtext node attr before_validate hooks on all richtext fields in the data map.
/// Used by both `LuaWriteHooks` and `update_many`.
/// Walks the field tree to find richtext fields with custom nodes, then runs
/// `run_before_validate_on_node_attrs` on each field's content.
pub(crate) fn apply_richtext_before_validate(
    lua: &mlua::Lua,
    fields: &[FieldDefinition],
    data: &mut std::collections::HashMap<String, Value>,
    registry: &Registry,
    collection: &str,
) {
    let richtext_fields = collect_richtext_fields(fields, "");

    if richtext_fields.is_empty() {
        return;
    }

    let has_any_hooks = richtext_fields.iter().any(|(f, _)| {
        f.admin.nodes.iter().any(|node_name| {
            registry
                .get_richtext_node(node_name)
                .map(|nd| nd.attrs.iter().any(|a| !a.hooks.before_validate.is_empty()))
                .unwrap_or(false)
        })
    });

    if !has_any_hooks {
        return;
    }

    for (field, data_key) in &richtext_fields {
        if let Some(Value::String(content)) = data.get(data_key.as_str()) {
            let new_content =
                run_before_validate_on_node_attrs(lua, content, field, registry, collection);
            if new_content != *content {
                data.insert(data_key.clone(), Value::String(new_content));
            }
        }
    }
}

/// Walk the field tree and collect richtext fields with custom nodes.
fn collect_richtext_fields<'a>(
    fields: &'a [FieldDefinition],
    prefix: &str,
) -> Vec<(&'a FieldDefinition, String)> {
    let mut out = Vec::new();

    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);
                out.extend(collect_richtext_fields(&field.fields, &new_prefix));
            }
            FieldType::Row | FieldType::Collapsible => {
                out.extend(collect_richtext_fields(&field.fields, prefix));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    out.extend(collect_richtext_fields(&tab.fields, prefix));
                }
            }
            FieldType::Richtext if !field.admin.nodes.is_empty() => {
                out.push((field, prefixed_name(prefix, &field.name)));
            }
            _ => {}
        }
    }

    out
}
