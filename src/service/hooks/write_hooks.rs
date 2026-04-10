//! `WriteHooks` trait and implementations for abstracting write hook execution
//! across different API surfaces (pool-based vs inline Lua VM).

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{Document, FieldDefinition, Registry, collection::Hooks},
    db::{AccessResult, DbConnection},
    hooks::{
        HookContext, HookEvent, HookRunner, ValidationCtx,
        lifecycle::{
            FieldHookEvent,
            access::{
                check_access_with_lua, check_field_read_access_with_lua,
                check_field_write_access_with_lua,
            },
            run_field_hooks_inner, run_hooks_inner, validate_fields_inner,
        },
    },
};

use super::richtext::apply_richtext_before_validate;

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
        data: Option<&HashMap<String, Value>>,
    ) -> Result<AccessResult>;

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
    /// When true, all access checks return Allowed unconditionally.
    /// Used by MCP (trusted local transport) to bypass access control.
    pub override_access: bool,
}

impl<'a> RunnerWriteHooks<'a> {
    /// Create with hooks enabled and no field access connection (the common case).
    pub fn new(runner: &'a HookRunner) -> Self {
        Self {
            runner,
            hooks_enabled: true,
            conn: None,
            override_access: false,
        }
    }

    /// Set the connection for field-level access checks.
    pub fn with_conn(mut self, conn: &'a dyn DbConnection) -> Self {
        self.conn = Some(conn);
        self
    }

    /// Set whether hooks are enabled.
    pub fn with_hooks_enabled(mut self, hooks_enabled: bool) -> Self {
        self.hooks_enabled = hooks_enabled;
        self
    }

    /// Bypass all access checks (returns Allowed unconditionally).
    /// Used by MCP tools which run on a trusted local transport.
    pub fn with_override_access(mut self) -> Self {
        self.override_access = true;
        self
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
        if self.override_access {
            return Vec::new();
        }
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
        data: Option<&HashMap<String, Value>>,
    ) -> Result<AccessResult> {
        if self.override_access {
            return Ok(AccessResult::Allowed);
        }
        let Some(conn) = self.conn else {
            return Ok(AccessResult::Allowed);
        };
        self.runner.check_access(access_ref, user, id, data, conn)
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

impl<'a> LuaWriteHooks<'a> {
    /// Create a builder with the required Lua VM reference.
    pub fn builder(lua: &'a mlua::Lua) -> LuaWriteHooksBuilder<'a> {
        LuaWriteHooksBuilder::new(lua)
    }
}

/// Builder for [`LuaWriteHooks`]. Created via [`LuaWriteHooks::builder`].
pub struct LuaWriteHooksBuilder<'a> {
    pub(in crate::service) lua: &'a mlua::Lua,
    pub(in crate::service) user: Option<&'a Document>,
    pub(in crate::service) ui_locale: Option<&'a str>,
    pub(in crate::service) override_access: bool,
    pub(in crate::service) registry: Option<&'a Registry>,
    pub(in crate::service) hooks_enabled: bool,
    pub(in crate::service) run_validation: bool,
}

impl<'a> LuaWriteHooksBuilder<'a> {
    pub fn new(lua: &'a mlua::Lua) -> Self {
        Self {
            lua,
            user: None,
            ui_locale: None,
            override_access: false,
            registry: None,
            hooks_enabled: true,
            run_validation: true,
        }
    }

    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<&'a str>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    pub fn override_access(mut self, override_access: bool) -> Self {
        self.override_access = override_access;
        self
    }

    pub fn registry(mut self, registry: Option<&'a Registry>) -> Self {
        self.registry = registry;
        self
    }

    pub fn hooks_enabled(mut self, hooks_enabled: bool) -> Self {
        self.hooks_enabled = hooks_enabled;
        self
    }

    pub fn run_validation(mut self, run_validation: bool) -> Self {
        self.run_validation = run_validation;
        self
    }

    pub fn build(self) -> LuaWriteHooks<'a> {
        LuaWriteHooks {
            lua: self.lua,
            user: self.user,
            ui_locale: self.ui_locale,
            override_access: self.override_access,
            registry: self.registry,
            hooks_enabled: self.hooks_enabled,
            run_validation: self.run_validation,
        }
    }
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
        data: Option<&HashMap<String, Value>>,
    ) -> Result<AccessResult> {
        if self.override_access {
            return Ok(AccessResult::Allowed);
        }
        check_access_with_lua(self.lua, access_ref, user, id, data)
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
