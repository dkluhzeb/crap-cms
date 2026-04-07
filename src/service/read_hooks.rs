//! `ReadHooks` trait and implementations for abstracting hook execution
//! across different API surfaces (pool-based vs inline Lua VM).

use anyhow::Result;

use crate::{
    core::{Document, FieldDefinition, collection::Hooks},
    db::DbConnection,
    hooks::{
        HookRunner,
        lifecycle::{
            AfterReadCtx, HookContext, HookEvent,
            access::check_field_read_access_with_lua,
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
    /// Fire before_read hooks. Returns error to abort the read.
    fn before_read(&self, hooks: &Hooks, slug: &str, operation: &str) -> Result<()>;

    /// Apply after_read hooks to a single document.
    fn after_read_one(&self, ctx: &AfterReadCtx, doc: Document) -> Document;

    /// Apply after_read hooks to a batch of documents.
    /// Default implementation calls `after_read_one` per document.
    fn after_read_many(&self, ctx: &AfterReadCtx, docs: Vec<Document>) -> Vec<Document> {
        docs.into_iter()
            .map(|d| self.after_read_one(ctx, d))
            .collect()
    }

    /// Return field names denied by read access control.
    /// Returns empty vec if access control is overridden.
    fn field_read_denied(&self, fields: &[FieldDefinition], user: Option<&Document>) -> Vec<String>;
}

/// Pool-based hook execution for admin, gRPC, and MCP surfaces.
/// Acquires a Lua VM from the HookRunner pool for each operation.
pub struct RunnerReadHooks<'a> {
    pub runner: &'a HookRunner,
    pub conn: &'a dyn DbConnection,
}

impl ReadHooks for RunnerReadHooks<'_> {
    fn before_read(&self, hooks: &Hooks, slug: &str, operation: &str) -> Result<()> {
        self.runner
            .fire_before_read(hooks, slug, operation, std::collections::HashMap::new())
    }

    fn after_read_one(&self, ctx: &AfterReadCtx, doc: Document) -> Document {
        self.runner.apply_after_read(ctx, doc)
    }

    fn after_read_many(&self, ctx: &AfterReadCtx, docs: Vec<Document>) -> Vec<Document> {
        self.runner.apply_after_read_many(ctx, docs)
    }

    fn field_read_denied(&self, fields: &[FieldDefinition], user: Option<&Document>) -> Vec<String> {
        self.runner.check_field_read_access(fields, user, self.conn)
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

impl ReadHooks for LuaReadHooks<'_> {
    fn before_read(&self, hooks: &Hooks, slug: &str, operation: &str) -> Result<()> {
        let ctx = HookContext::builder(slug, operation)
            .user(self.user)
            .ui_locale(self.ui_locale)
            .build();
        run_hooks_inner(self.lua, hooks, HookEvent::BeforeRead, ctx)?;
        Ok(())
    }

    fn after_read_one(&self, ctx: &AfterReadCtx, doc: Document) -> Document {
        apply_after_read_inner(self.lua, ctx, doc)
    }

    fn field_read_denied(&self, fields: &[FieldDefinition], user: Option<&Document>) -> Vec<String> {
        if self.override_access {
            return Vec::new();
        }
        check_field_read_access_with_lua(self.lua, fields, user)
    }
}
