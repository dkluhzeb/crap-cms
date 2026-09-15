//! `WriteHooks` trait and implementations for abstracting write hook execution
//! across different API surfaces (pool-based vs inline Lua VM).

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{
        Builder, Document, DocumentFields, FieldDefinition, Hooks, Registry, ValidationError,
        nest_group_fields,
    },
    db::{AccessResult, DbConnection, LocaleContext, query::helpers::column_belongs_to},
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
    service::hooks::FieldReadStrip,
};

/// Local alias to disambiguate from the file-wide `anyhow::Result`.
type ValidateResult = std::result::Result<(), ValidationError>;

use super::richtext::apply_richtext_before_validate;

/// Drop the columns that belong to every field a write strip removed from a
/// version snapshot: its per-locale columns (`price__en`, `seo__title__de`) and
/// a date's timezone companion (`starts_tz`, `starts_tz__de`).
///
/// Snapshots carry these beside a field's resolved value, and restore writes
/// them. The strip removes only the resolved key, so without this a
/// write-denied field would still be overwritten through its companions.
/// Handles companions kept at the top level and inside nested group objects
/// alike.
fn drop_locale_columns_of_stripped(before: &Map<String, Value>, after: &mut Map<String, Value>) {
    let mut removed = Vec::new();
    collect_removed_paths(before, after, "", &mut removed);

    if removed.is_empty() {
        return;
    }

    drop_decorated(after, "", &removed);
}

/// Collect the `__`-joined path of every key present in `before` but missing
/// from `after`, descending into objects present in both.
fn collect_removed_paths(
    before: &Map<String, Value>,
    after: &Map<String, Value>,
    prefix: &str,
    removed: &mut Vec<String>,
) {
    for (key, value) in before {
        let path = format!("{prefix}{key}");

        match (value, after.get(key)) {
            (_, None) => removed.push(path),
            (Value::Object(b), Some(Value::Object(a))) => {
                collect_removed_paths(b, a, &format!("{path}__"), removed);
            }
            _ => {}
        }
    }
}

/// Remove every key whose `__`-joined path belongs to one of the `removed`
/// field paths, at this level and in nested objects.
fn drop_decorated(level: &mut Map<String, Value>, prefix: &str, removed: &[String]) {
    level.retain(|key, _| {
        let path = format!("{prefix}{key}");
        !removed.iter().any(|field| column_belongs_to(&path, field))
    });

    for (key, value) in level.iter_mut() {
        if let Value::Object(nested) = value {
            drop_decorated(nested, &format!("{prefix}{key}__"), removed);
        }
    }
}

/// Shared body of the create/update strips: round-trip `data` through the
/// map-level strip, with `input.document` as `ctx.document`.
fn strip_in_place<H: WriteHooks + ?Sized>(
    hooks: &H,
    fields: &[FieldDefinition],
    data: &mut DocumentFields,
    input: &WriteStripInput<'_>,
) {
    let mut level: Map<String, Value> = std::mem::take(data).into_inner().into_iter().collect();

    hooks.strip_write_access_map(fields, &mut level, input);

    *data = level.into_iter().collect();
}

/// Trait for executing write hooks, abstracting over VM acquisition strategy.
///
/// Two implementations exist:
/// - [`RunnerWriteHooks`]: acquires a Lua VM from the pool (admin, gRPC, MCP)
/// - [`LuaWriteHooks`]: uses the current Lua VM inline (Lua CRUD hooks)
///
/// The data-aware field-read strip a write applies to the document it reports
/// lives in [`FieldReadStrip`], shared with the read surface so both strip the
/// same way.
pub trait WriteHooks: FieldReadStrip {
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
    /// may not write under `input.operation` (`"create"` | `"update"`),
    /// evaluating each `access.create` / `access.update` rule with `ctx.data` =
    /// the field's own immediate level and `ctx.document` = `input.document`
    /// (the stored document on update, the incoming one on create). The
    /// write-path mirror of `strip_read_access_map`. Default no-op so
    /// lightweight test/override impls keep their behavior.
    fn strip_write_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        input: &WriteStripInput<'_>,
    ) {
        let _ = (fields, level, input);
    }

    /// Strip create-denied fields from incoming [`DocumentFields`] in place.
    /// No row exists yet, so each `access.create` rule sees the full pre-strip
    /// incoming data as `ctx.document`.
    fn strip_write_access_create(
        &self,
        fields: &[FieldDefinition],
        data: &mut DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if !has_any_field_access(fields, |f| f.access.create.as_ref()) {
            return;
        }

        let document = data.clone();
        strip_in_place(
            self,
            fields,
            data,
            &WriteStripInput {
                document: &document,
                collection,
                user,
                locale,
                operation: "create",
            },
        );
    }

    /// Strip update-denied fields from an incoming patch in place.
    ///
    /// Each `access.update` rule sees the STORED document as `ctx.document`.
    /// The patch is what the rule is judging, so it cannot also be the
    /// evidence: a rule gating a field on `ctx.document.owner` would otherwise
    /// pass for any caller who puts their own id in `owner` in the same write.
    /// Callers load `stored` with [`stored_fields_for_update_rules`] (or its
    /// global twin); an empty document makes a stored-value rule deny.
    ///
    /// [`stored_fields_for_update_rules`]: crate::service::stored_fields_for_update_rules
    fn strip_write_access_update(
        &self,
        fields: &[FieldDefinition],
        data: &mut DocumentFields,
        stored: &DocumentFields,
        collection: &str,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if !has_any_field_access(fields, |f| f.access.update.as_ref()) {
            return;
        }

        let document = nest_group_fields(stored, fields);
        strip_in_place(
            self,
            fields,
            data,
            &WriteStripInput {
                document: &document,
                collection,
                user,
                locale,
                operation: "update",
            },
        );
    }

    /// Strip **write**-denied fields from a version-snapshot `Value::Object` in
    /// place (no-op for a non-object snapshot), evaluating each field's
    /// `access.update` rule. Used by the version-restore path: a user who may
    /// `update` a document but is write-denied on a specific field must not be
    /// able to use a restore to overwrite that field's live value. The denied
    /// field is dropped from the snapshot — with its per-locale columns and a
    /// date's timezone companion — so the partial restore leaves its stored
    /// value untouched (the same input-stripping model `update` uses).
    ///
    /// Each rule judges `stored` — the live row — as `ctx.document`, exactly as
    /// an update does: the snapshot is the value under judgment, so it cannot
    /// also be the evidence. Mirrors
    /// [`FieldReadStrip::strip_read_access_doc`].
    fn strip_write_access_value(
        &self,
        fields: &[FieldDefinition],
        snapshot: &mut Value,
        stored: &DocumentFields,
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

        let before = level.clone();
        let document = nest_group_fields(stored, fields);

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

        drop_locale_columns_of_stripped(&before, &mut level);

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

/// Inline Lua VM write hook execution for Lua CRUD hooks.
///
/// No `user`/`ui_locale` here: hook contexts get both from the service
/// context (`ctx.user` / the write input), and access checks receive the user
/// via [`AccessCheckInput`] — the fields existed once, were never read, and
/// every codec dutifully filled them for no effect.
#[derive(Builder)]
pub struct LuaWriteHooks<'a> {
    #[builder(required)]
    pub lua: &'a mlua::Lua,
    pub override_access: bool,
    pub registry: Option<&'a Registry>,
    /// Whether hooks are enabled (false when hook depth exceeded or `hooks: false` option).
    #[builder(default = true)]
    pub hooks_enabled: bool,
    /// Whether validation should run (`hooks` option from Lua API).
    #[builder(default = true)]
    pub run_validation: bool,
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

impl FieldReadStrip for LuaWriteHooks<'_> {
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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Regression: a stripped field's companion columns were dropped for
    /// per-locale and timezone keys only, so a denied code field kept its
    /// `_lang` companion in the write.
    #[test]
    fn a_stripped_code_field_drops_its_language_companion() {
        let before = json!({ "snippet": "x", "snippet_lang": "rust", "title": "t" });
        let mut after = json!({ "snippet_lang": "rust", "title": "t" });

        drop_locale_columns_of_stripped(
            before.as_object().unwrap(),
            after.as_object_mut().unwrap(),
        );

        assert_eq!(after, json!({ "title": "t" }));
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!("fixture is an object"),
        }
    }

    /// A stripped localized field must lose its decorated columns too, or a
    /// restore writes them back.
    #[test]
    fn stripped_fields_lose_their_locale_columns() {
        let before = object(json!({
            "price": 10, "price__en": 10, "price__de": 12,
            "title": "t", "title__en": "t",
            "starts": "2024-01-01T10:00", "starts_tz": "Europe/Berlin",
            "starts_tz__de": "Europe/Berlin", "starts_at_home": "kept",
            "seo": { "title": "x", "title__en": "x", "desc": "d" },
            "seo__title__de": "y",
        }));
        let mut after = before.clone();
        after.remove("price");
        after.remove("starts");
        after["seo"].as_object_mut().unwrap().remove("title");

        drop_locale_columns_of_stripped(&before, &mut after);

        assert_eq!(
            Value::Object(after),
            json!({
                "title": "t", "title__en": "t",
                "starts_at_home": "kept",
                "seo": { "desc": "d" },
            })
        );
    }
}
