//! Pool-based [`WriteHooks`] for the admin, gRPC, and MCP surfaces.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{Document, DocumentFields, FieldDefinition, Hooks, Registry},
    db::{AccessResult, DbConnection},
    hooks::{
        HookContext, HookEvent, HookRunner, ValidationCtx,
        lifecycle::{
            AccessCheckInput, LuaCrudInfra,
            access::{ReadStripInput, WriteStripInput},
        },
    },
    service::hooks::FieldReadStrip,
};

use super::{ValidateResult, WriteHooks};

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
    /// Infrastructure for Lua CRUD event publishing, cache invalidation, and event
    /// queueing. Threaded into the Lua VM so that CRUD calls from hooks can publish
    /// events and clear the cache.
    pub infra: Option<LuaCrudInfra>,
}

impl<'a> RunnerWriteHooks<'a> {
    /// Create with hooks enabled and no field access connection (the common case).
    #[must_use]
    pub fn new(runner: &'a HookRunner) -> Self {
        Self {
            runner,
            hooks_enabled: true,
            conn: None,
            override_access: false,
            infra: None,
        }
    }

    /// Set the connection for field-level access checks.
    #[must_use]
    pub fn with_conn(mut self, conn: &'a dyn DbConnection) -> Self {
        self.conn = Some(conn);
        self
    }

    /// Set whether hooks are enabled.
    #[must_use]
    pub fn with_hooks_enabled(mut self, hooks_enabled: bool) -> Self {
        self.hooks_enabled = hooks_enabled;
        self
    }

    /// Bypass all access checks (returns Allowed unconditionally).
    /// Used by MCP tools which run on a trusted local transport.
    #[must_use]
    pub fn with_override_access(mut self) -> Self {
        self.override_access = true;
        self
    }

    /// Attach infrastructure for Lua CRUD event/cache operations.
    #[must_use]
    pub fn with_infra(mut self, infra: LuaCrudInfra) -> Self {
        self.infra = Some(infra);
        self
    }
}

impl WriteHooks for RunnerWriteHooks<'_> {
    fn runs_delete_hooks(&self, hooks: &Hooks) -> bool {
        self.hooks_enabled
            && (!hooks.before_delete.is_empty()
                || !hooks.after_delete.is_empty()
                || self.runner.has_registered_hooks_for("before_delete")
                || self.runner.has_registered_hooks_for("after_delete"))
    }

    fn registry(&self) -> Option<&Registry> {
        Some(self.runner.registry())
    }

    fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext> {
        if self.hooks_enabled {
            self.runner
                .run_before_write(hooks, fields, ctx, val_ctx, self.infra.clone())
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
            self.runner
                .run_after_write(hooks, fields, event, ctx, conn, self.infra.clone())
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
            self.runner
                .run_hooks_with_conn(hooks, event, ctx, conn, self.infra.clone())
        } else {
            Ok(ctx)
        }
    }

    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        if self.override_access {
            return Ok(AccessResult::Allowed);
        }
        let Some(conn) = self.conn else {
            return Ok(AccessResult::Allowed);
        };
        self.runner.check_access(input, conn)
    }

    fn strip_write_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        input: &WriteStripInput<'_>,
    ) {
        if self.override_access {
            return;
        }
        let Some(conn) = self.conn else {
            return;
        };
        self.runner.strip_write_access(fields, level, input, conn);
    }

    fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &DocumentFields,
        ctx: &ValidationCtx,
    ) -> ValidateResult {
        self.runner.validate_fields(fields, data, ctx)
    }
}

impl FieldReadStrip for RunnerWriteHooks<'_> {
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        document: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if self.override_access {
            return;
        }
        let Some(conn) = self.conn else {
            return;
        };
        let input = ReadStripInput {
            document,
            collection,
            user,
            locale,
        };
        self.runner.strip_read_access(fields, level, &input, conn);
    }
}
