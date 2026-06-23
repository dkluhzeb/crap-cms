//! `WriteHooks` trait and implementations for abstracting write hook execution
//! across different API surfaces (pool-based vs inline Lua VM).

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{
        Document, DocumentFields, FieldDefinition, Hooks, Registry, ValidationError,
        nest_group_fields,
    },
    db::{AccessResult, DbConnection, LocaleContext},
    hooks::{
        HookContext, HookEvent, HookRunner, ValidationCtx,
        lifecycle::{
            AccessCheckInput, FieldHookEvent, FieldHooksCall, LuaCrudInfra,
            access::{
                ReadStripInput, WriteStripInput, check_collection_access, has_any_field_access,
                strip_read_access_with_lua, strip_write_access_with_lua,
            },
            run_field_hooks_inner, run_hooks_inner, validate_fields_inner,
        },
    },
};

/// Local alias to disambiguate from the file-wide `anyhow::Result`.
type ValidateResult = std::result::Result<(), ValidationError>;

use super::richtext::apply_richtext_before_validate;

/// Trait for executing write hooks, abstracting over VM acquisition strategy.
///
/// Two implementations exist:
/// - [`RunnerWriteHooks`]: acquires a Lua VM from the pool (admin, gRPC, MCP)
/// - [`LuaWriteHooks`]: uses the current Lua VM inline (Lua CRUD hooks)
pub trait WriteHooks {
    /// Full before-write pipeline: field `BeforeValidate` → richtext attr hooks →
    /// collection `BeforeValidate` → validate → field `BeforeChange` → collection `BeforeChange`.
    ///
    /// # Errors
    ///
    /// Returns an error if any hook stage or validation fails.
    fn run_before_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        ctx: HookContext,
        val_ctx: &ValidationCtx,
    ) -> Result<HookContext>;

    /// After-write hooks: field `AfterChange` → collection `AfterChange` → registered hooks.
    ///
    /// # Errors
    ///
    /// Returns an error if any hook execution fails.
    fn run_after_write(
        &self,
        hooks: &Hooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext>;

    /// Run collection-level hooks with CRUD access (for `BeforeDelete` / `AfterDelete`).
    ///
    /// # Errors
    ///
    /// Returns an error if any hook execution fails.
    fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        ctx: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext>;

    /// Whether a `before_delete` / `after_delete` hook will actually run for a
    /// collection with these `hooks`. Callers use it to decide whether to
    /// pre-load the document's field data into the delete-hook context — so the
    /// load is skipped when no delete hook would see it.
    ///
    /// Default: true when the collection declares any delete hook. Surface
    /// impls refine this with their `hooks_enabled` flag (and, for the pool
    /// runner, knowledge of globally-registered delete hooks).
    fn runs_delete_hooks(&self, hooks: &Hooks) -> bool {
        !hooks.before_delete.is_empty() || !hooks.after_delete.is_empty()
    }

    /// Data-aware field-**read** strip applied to a write op's returned document.
    /// See [`crate::service::hooks::ReadHooks::strip_read_access_map`]. Default
    /// no-op so lightweight test/override impls keep their behavior.
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        document: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        let _ = (fields, level, document, collection, user, locale);
    }

    /// Convenience: strip read-denied fields from a returned [`Document`] in
    /// place, capturing the full pre-strip document as `ctx.document`.
    fn strip_read_access_doc(
        &self,
        fields: &[FieldDefinition],
        doc: &mut Document,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        // Skip the per-document clone + map round-trip entirely when no field
        // configures read access (the common case) — matches the read path.
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return;
        }

        let document = doc.fields.clone();
        let mut level: Map<String, Value> = std::mem::take(&mut doc.fields)
            .into_inner()
            .into_iter()
            .collect();

        self.strip_read_access_map(fields, &mut level, &document, collection, user, locale);

        doc.fields = level.into_iter().collect();
    }

    /// Collection-level access check. Returns the access result (Allowed/Denied/Constrained).
    ///
    /// `locale` is the locale this operation targets (the resolved/default
    /// locale, or `None` when localization is disabled or the op is
    /// locale-agnostic), exposed to access functions as `context.locale`.
    ///
    /// # Errors
    ///
    /// Returns an error if the access hook itself raises (e.g. a Lua runtime error).
    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult>;

    /// Data-aware field-**write** strip: remove from `level` every field the user
    /// may not write under `operation` (`"create"` | `"update"`), evaluating each
    /// `access.create` / `access.update` rule with `ctx.data` = the field's own
    /// immediate level and `ctx.document` = `document` (the full incoming
    /// document). The write-path mirror of `strip_read_access_map`. Default no-op
    /// so lightweight test/override impls keep their behavior.
    fn strip_write_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        input: &WriteStripInput<'_>,
    ) {
        let _ = (fields, level, input);
    }

    /// Convenience over [`strip_write_access_map`](Self::strip_write_access_map):
    /// strip write-denied fields from incoming [`DocumentFields`] in place before
    /// persistence, capturing the full pre-strip data as `ctx.document`.
    fn strip_write_access_data(
        &self,
        fields: &[FieldDefinition],
        data: &mut DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
        operation: &str,
    ) {
        // Skip the clone + map round-trip when no field configures the relevant
        // write-access function (or the operation isn't create/update).
        let has_access = match operation {
            "create" => has_any_field_access(fields, |f| f.access.create.as_ref()),
            "update" => has_any_field_access(fields, |f| f.access.update.as_ref()),
            _ => return,
        };
        if !has_access {
            return;
        }

        let document = data.clone();
        let mut level: Map<String, Value> = std::mem::take(data).into_inner().into_iter().collect();

        self.strip_write_access_map(
            fields,
            &mut level,
            &WriteStripInput {
                document: &document,
                collection,
                user,
                locale,
                operation,
            },
        );

        *data = level.into_iter().collect();
    }

    /// Strip **write**-denied fields from a version-snapshot `Value::Object` in
    /// place (no-op for a non-object snapshot), evaluating each field's
    /// `access.update` rule. Used by the version-restore path: a user who may
    /// `update` a document but is write-denied on a specific field must not be
    /// able to use a restore to overwrite that field's live value. The denied
    /// field is dropped from the snapshot, so the partial restore leaves its
    /// stored value untouched (the same input-stripping model `update` uses).
    /// The snapshot is its own `ctx.document`. Mirrors
    /// [`ReadHooks::strip_read_access_value`](crate::service::hooks::ReadHooks::strip_read_access_value).
    fn strip_write_access_value(
        &self,
        fields: &[FieldDefinition],
        snapshot: &mut Value,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if !has_any_field_access(fields, |f| f.access.update.as_ref()) {
            return;
        }

        let Some(obj) = snapshot.as_object() else {
            return;
        };

        // Legacy snapshots may be stored in the flat `group__sub` column form,
        // but the data-aware strip walks the canonical nested shape. Normalize
        // first so a write-denied group sub-field is stripped regardless of how
        // the snapshot was stored; `nest_group_fields` is idempotent for the
        // nested snapshots current code writes. Mirrors the read-path variant.
        let nested = nest_group_fields(&obj.clone().into_iter().collect(), fields);
        let mut level: Map<String, Value> = nested.into_inner().into_iter().collect();

        let document: DocumentFields = level.clone().into_iter().collect();

        self.strip_write_access_map(
            fields,
            &mut level,
            &WriteStripInput {
                document: &document,
                collection,
                user,
                locale,
                operation: "update",
            },
        );

        *snapshot = Value::Object(level);
    }

    /// Run schema-level field validation (required, unique, regex, type checks,
    /// richtext node attrs, …) without firing any user-defined hooks. Used by
    /// the version restore path so a snapshot whose data violates the current
    /// schema (e.g. an old version from before a `required = true` tightening)
    /// is rejected rather than silently overwriting valid live data.
    ///
    /// # Errors
    ///
    /// Returns a `ValidationError` (containing per-field error messages) when
    /// any schema check fails. Never raises a runtime/IO error — `ValidateResult`
    /// is a `Result<(), ValidationError>` purely as a structured way to surface
    /// the collected errors.
    fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &DocumentFields,
        ctx: &ValidationCtx,
    ) -> ValidateResult;
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
    #[must_use]
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
    fn runs_delete_hooks(&self, hooks: &Hooks) -> bool {
        // Inline nested-Lua delete path: gate on the hooks-enabled flag and the
        // collection's own delete hooks. Globally-registered delete hooks in
        // the current VM are not probed here (the common case is collection
        // refs); a registered-only delete hook still gets the `id`.
        self.hooks_enabled && (!hooks.before_delete.is_empty() || !hooks.after_delete.is_empty())
    }

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
                &mut ctx.data,
                &FieldHooksCall {
                    fields,
                    event: FieldHookEvent::BeforeValidate,
                    collection: &ctx.collection,
                    operation: &ctx.operation,
                    id: ctx.document_id.as_deref(),
                    locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
                },
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
                &mut ctx.data,
                &FieldHooksCall {
                    fields,
                    event: FieldHookEvent::BeforeChange,
                    collection: &ctx.collection,
                    operation: &ctx.operation,
                    id: ctx.document_id.as_deref(),
                    locale: val_ctx.locale_ctx.map(LocaleContext::access_locale),
                },
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
                &mut ctx.data,
                &FieldHooksCall {
                    fields,
                    event: FieldHookEvent::AfterChange,
                    collection: &ctx.collection,
                    operation: &ctx.operation,
                    id: ctx.document_id.as_deref(),
                    locale: ctx.locale.as_deref(),
                },
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

    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        if self.override_access {
            return Ok(AccessResult::Allowed);
        }
        // Route through the chokepoint (not the bare evaluator) so a write
        // hook's row constraints get the same operator / dotted / locale
        // validation as every other surface.
        check_collection_access(self.lua, input)
    }

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
        let input = ReadStripInput {
            document,
            collection,
            user,
            locale,
        };
        strip_read_access_with_lua(self.lua, fields, level, &input);
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
        strip_write_access_with_lua(self.lua, fields, level, input);
    }

    fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &DocumentFields,
        ctx: &ValidationCtx,
    ) -> ValidateResult {
        validate_fields_inner(self.lua, fields, data, ctx)
    }
}
