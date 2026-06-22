//! `ReadHooks` trait and implementations for abstracting hook execution
//! across different API surfaces (pool-based vs inline Lua VM).

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{Document, DocumentFields, FieldDefinition, HookRef, collection::Hooks},
    db::{AccessResult, DbConnection, query::JoinAccessCheck},
    hooks::{
        HookRunner,
        lifecycle::{
            AccessCheckInput, AfterReadCtx, HookContext, HookEvent,
            access::{check_collection_access, has_any_field_access, strip_read_access_with_lua},
            apply_after_read_inner, run_hooks_inner,
        },
    },
};

/// Trait for executing read hooks, abstracting over VM acquisition strategy.
///
/// Two implementations exist:
/// - [`RunnerReadHooks`]: acquires a Lua VM from the pool (admin, gRPC, MCP)
/// - [`LuaReadHooks`]: uses the current Lua VM inline (Lua CRUD hooks)
pub trait ReadHooks {
    /// Fire `before_read` hooks. Returns error to abort the read.
    ///
    /// # Errors
    ///
    /// Returns an error if any `before_read` hook fails or aborts the read.
    fn before_read(
        &self,
        hooks: &Hooks,
        slug: &str,
        operation: &str,
        locale: Option<&str>,
    ) -> Result<()>;

    /// Apply `after_read` hooks to a single document.
    fn after_read_one(&self, ctx: &AfterReadCtx, doc: Document) -> Document;

    /// Apply `after_read` hooks to a batch of documents.
    /// Default implementation calls `after_read_one` per document.
    fn after_read_many(&self, ctx: &AfterReadCtx, docs: Vec<Document>) -> Vec<Document> {
        docs.into_iter()
            .map(|d| self.after_read_one(ctx, d))
            .collect()
    }

    /// Check collection-level access. Returns the access result (Allowed/Denied/Constrained).
    ///
    /// `locale` is the locale this read targets (resolved/default, or `None`
    /// when localization is disabled), exposed as `context.locale`.
    ///
    /// # Errors
    ///
    /// Returns an error if the access hook itself raises (e.g. a Lua runtime error).
    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult>;

    /// Data-aware field-**read** strip: remove read-denied fields from `level`
    /// in place, evaluating each `access.read` rule with `ctx.data` = the field's
    /// own immediate level (the row, for fields inside an array/blocks row) and
    /// `ctx.document` = `document` (the full document). The universal `Map` form
    /// covers documents, version snapshots, populated targets, and live events.
    ///
    /// Default no-op so the many lightweight test/override `ReadHooks` impls that
    /// don't enforce field access keep their behavior; the real surfaces
    /// ([`RunnerReadHooks`], [`LuaReadHooks`]) override it.
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        document: &DocumentFields,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        let _ = (fields, level, document, user, locale);
    }

    /// Convenience over [`strip_read_access_map`](Self::strip_read_access_map):
    /// strip read-denied fields from a [`Document`] in place, capturing the full
    /// pre-strip document as `ctx.document`.
    fn strip_read_access_doc(
        &self,
        fields: &[FieldDefinition],
        doc: &mut Document,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        // Skip the per-document clone + map round-trip entirely when no field
        // configures read access (the common case) — the read hot path pays nothing.
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return;
        }

        let document = doc.fields.clone();
        let mut level: Map<String, Value> = std::mem::take(&mut doc.fields)
            .into_inner()
            .into_iter()
            .collect();

        self.strip_read_access_map(fields, &mut level, &document, user, locale);

        doc.fields = level.into_iter().collect();
    }

    /// Batched form of [`strip_read_access_doc`](Self::strip_read_access_doc)
    /// for a list read. The default loops per document; [`RunnerReadHooks`]
    /// overrides it to acquire the Lua VM **once** for the whole batch (the
    /// per-query perf model) instead of once per document. Each document is
    /// still stripped against its own `ctx.document` / per-row `ctx.data`.
    fn strip_read_access_docs(
        &self,
        fields: &[FieldDefinition],
        docs: &mut [Document],
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        for doc in docs.iter_mut() {
            self.strip_read_access_doc(fields, doc, user, locale);
        }
    }

    /// Convenience over [`strip_read_access_map`](Self::strip_read_access_map):
    /// strip read-denied fields from a version-snapshot `Value::Object` in place
    /// (no-op for a non-object snapshot). The snapshot is its own `ctx.document`.
    fn strip_read_access_value(
        &self,
        fields: &[FieldDefinition],
        snapshot: &mut Value,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if !has_any_field_access(fields, |f| f.access.read.as_ref()) {
            return;
        }

        let Some(level) = snapshot.as_object_mut() else {
            return;
        };
        let document: DocumentFields = level.clone().into_iter().collect();

        self.strip_read_access_map(fields, level, &document, user, locale);
    }
}

/// Pool-based hook execution for admin, gRPC, and MCP surfaces.
/// Acquires a Lua VM from the `HookRunner` pool for each operation.
pub struct RunnerReadHooks<'a> {
    pub runner: &'a HookRunner,
    pub conn: &'a dyn DbConnection,
    /// The authenticated user, exposed to `before_read` hooks as `ctx.user`.
    pub user: Option<&'a Document>,
    /// The admin UI locale, exposed to `before_read` hooks as `ctx.ui_locale`.
    pub ui_locale: Option<&'a str>,
    /// Bypass collection- and field-level read access entirely. The MCP
    /// full-access surface sets this so its reads match its writes (which
    /// already bypass via `RunnerWriteHooks`). Defaults to `false`.
    pub override_access: bool,
}

impl<'a> RunnerReadHooks<'a> {
    pub fn new(
        runner: &'a HookRunner,
        conn: &'a dyn DbConnection,
        user: Option<&'a Document>,
        ui_locale: Option<&'a str>,
    ) -> Self {
        Self {
            runner,
            conn,
            user,
            ui_locale,
            override_access: false,
        }
    }

    /// Opt into full-access reads (collection- and field-level access skipped).
    /// Used by the MCP surface, which operates as a single privileged token —
    /// mirrors [`RunnerWriteHooks::with_override_access`].
    #[must_use]
    pub fn with_override_access(mut self) -> Self {
        self.override_access = true;
        self
    }
}

impl ReadHooks for RunnerReadHooks<'_> {
    fn before_read(
        &self,
        hooks: &Hooks,
        slug: &str,
        operation: &str,
        locale: Option<&str>,
    ) -> Result<()> {
        let ctx = HookContext::builder(slug, operation)
            .user(self.user)
            .locale(locale)
            .ui_locale(self.ui_locale)
            .build();
        self.runner.fire_before_read(hooks, ctx)
    }

    fn after_read_one(&self, ctx: &AfterReadCtx, doc: Document) -> Document {
        self.runner.apply_after_read(ctx, doc)
    }

    fn after_read_many(&self, ctx: &AfterReadCtx, docs: Vec<Document>) -> Vec<Document> {
        self.runner.apply_after_read_many(ctx, docs)
    }

    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        if self.override_access {
            return Ok(AccessResult::Allowed);
        }
        self.runner.check_access(input, self.conn)
    }

    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        document: &DocumentFields,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if self.override_access {
            return;
        }
        self.runner
            .strip_read_access(fields, level, document, user, locale, self.conn);
    }

    fn strip_read_access_docs(
        &self,
        fields: &[FieldDefinition],
        docs: &mut [Document],
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if self.override_access {
            return;
        }
        self.runner
            .strip_read_access_batch(fields, docs, user, locale, self.conn);
    }
}

/// Inline Lua VM hook execution for Lua CRUD hooks.
/// Uses the current Lua VM directly (already inside a hook context).
pub struct LuaReadHooks<'a> {
    pub lua: &'a mlua::Lua,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
    pub override_access: bool,
}

impl<'a> LuaReadHooks<'a> {
    /// Create a builder with the required Lua VM reference.
    #[must_use]
    pub fn builder(lua: &'a mlua::Lua) -> LuaReadHooksBuilder<'a> {
        LuaReadHooksBuilder::new(lua)
    }
}

/// Builder for [`LuaReadHooks`]. Created via [`LuaReadHooks::builder`].
pub struct LuaReadHooksBuilder<'a> {
    pub(in crate::service) lua: &'a mlua::Lua,
    pub(in crate::service) user: Option<&'a Document>,
    pub(in crate::service) ui_locale: Option<&'a str>,
    pub(in crate::service) override_access: bool,
}

impl<'a> LuaReadHooksBuilder<'a> {
    pub fn new(lua: &'a mlua::Lua) -> Self {
        Self {
            lua,
            user: None,
            ui_locale: None,
            override_access: false,
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

    pub fn build(self) -> LuaReadHooks<'a> {
        LuaReadHooks {
            lua: self.lua,
            user: self.user,
            ui_locale: self.ui_locale,
            override_access: self.override_access,
        }
    }
}

/// Adapter that lets `populate` invoke a `ReadHooks` as a [`JoinAccessCheck`]
/// for join-field target-collection access enforcement (SEC-G).
pub(crate) struct ReadHooksJoinGuard<'a> {
    hooks: &'a dyn ReadHooks,
}

impl<'a> ReadHooksJoinGuard<'a> {
    pub fn new(hooks: &'a dyn ReadHooks) -> Self {
        Self { hooks }
    }
}

impl JoinAccessCheck for ReadHooksJoinGuard<'_> {
    /// A returned `Constrained` filter is not re-validated here: the underlying
    /// `check_access` already validates at the `check_collection_access`
    /// chokepoint (operators, system columns, locale-scoped fields), and populate
    /// matches the constraint in-memory against the raw target row — a malformed
    /// constraint can only over-restrict (drop a target), never over-expose.
    fn check(
        &self,
        access: Option<&HookRef>,
        user: Option<&Document>,
        collection: &str,
    ) -> anyhow::Result<AccessResult> {
        self.hooks.check_access(&AccessCheckInput {
            document: None,
            access,
            user,
            id: None,
            data: None,
            locale: None,
            operation: "find",
            collection,
            ui_locale: None,
        })
    }
}

impl ReadHooks for LuaReadHooks<'_> {
    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        if self.override_access {
            return Ok(AccessResult::Allowed);
        }
        check_collection_access(self.lua, input)
    }

    fn before_read(
        &self,
        hooks: &Hooks,
        slug: &str,
        operation: &str,
        locale: Option<&str>,
    ) -> Result<()> {
        let ctx = HookContext::builder(slug, operation)
            .user(self.user)
            .locale(locale)
            .ui_locale(self.ui_locale)
            .build();
        run_hooks_inner(self.lua, hooks, HookEvent::BeforeRead, ctx)?;
        Ok(())
    }

    fn after_read_one(&self, ctx: &AfterReadCtx, doc: Document) -> Document {
        apply_after_read_inner(self.lua, ctx, doc)
    }

    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        document: &DocumentFields,
        user: Option<&Document>,
        locale: Option<&str>,
    ) {
        if self.override_access {
            return;
        }
        strip_read_access_with_lua(self.lua, fields, level, document, user, locale);
    }
}
