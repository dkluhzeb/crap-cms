//! HookRunner core run methods: collection hooks, field hooks, system hooks.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{Document, FieldDefinition, collection::Hooks},
    db::DbConnection,
    hooks::{
        HookContext, HookEvent, HookRunner,
        lifecycle::{
            execution::{
                call_hook_ref, call_registered_hooks, get_hook_refs, has_field_hooks_for_event,
                run_field_hooks_inner,
            },
            types::{FieldHookEvent, TxContextGuard},
        },
    },
};

/// Bundled transaction context for field-level write hooks.
pub struct FieldWriteCtx<'a> {
    pub conn: &'a dyn DbConnection,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
}

impl<'a> FieldWriteCtx<'a> {
    /// Create a builder with the required connection reference.
    pub fn builder(conn: &'a dyn DbConnection) -> FieldWriteCtxBuilder<'a> {
        FieldWriteCtxBuilder::new(conn)
    }
}

/// Builder for [`FieldWriteCtx`].
pub struct FieldWriteCtxBuilder<'a> {
    conn: &'a dyn DbConnection,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

impl<'a> FieldWriteCtxBuilder<'a> {
    pub(crate) fn new(conn: &'a dyn DbConnection) -> Self {
        Self {
            conn,
            user: None,
            ui_locale: None,
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

    pub fn build(self) -> FieldWriteCtx<'a> {
        FieldWriteCtx {
            conn: self.conn,
            user: self.user,
            ui_locale: self.ui_locale,
        }
    }
}

impl HookRunner {
    /// Run all hooks for a given event, mutating the context.
    /// Runs collection-level hook refs first, then global registered hooks.
    /// Does NOT provide CRUD access to hooks (use `run_hooks_with_conn` for that).
    pub fn run_hooks(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        mut context: HookContext,
    ) -> Result<HookContext> {
        let hook_refs = get_hook_refs(hooks, &event);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for(event.as_str()) {
            return Ok(context);
        }

        let lua = self.pool.acquire()?;

        for hook_ref in hook_refs {
            tracing::debug!("Running hook: {} for {}", hook_ref, context.collection);
            context = call_hook_ref(&lua, hook_ref, context)?;
        }

        // Run global registered hooks
        context = call_registered_hooks(&lua, &event, context)?;

        Ok(context)
    }

    /// Run hooks with an active database connection/transaction injected.
    /// Runs collection-level hook refs first, then global registered hooks.
    /// CRUD functions (`crap.collections.find`, `.create`, etc.) become available
    /// to Lua hooks and share the provided connection for transaction atomicity.
    /// The authenticated user and UI locale are extracted from the `HookContext`.
    pub fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        mut context: HookContext,
        conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        let hook_refs = get_hook_refs(hooks, &event);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for(event.as_str()) {
            return Ok(context);
        }

        let lua = self.pool.acquire()?;

        // Inject the connection pointer so CRUD functions can use it.
        // Safety: conn is valid for the duration of this method, and we hold
        // the Lua mutex so no concurrent access is possible.
        // Guard cleans up TxContext, UserContext, and UiLocaleContext on drop.
        let _guard =
            TxContextGuard::set(&lua, conn, context.user.clone(), context.ui_locale.clone());

        for hook_ref in hook_refs {
            tracing::debug!("Running hook (tx): {} for {}", hook_ref, context.collection);
            context = call_hook_ref(&lua, hook_ref, context)?;
        }

        // Run global registered hooks (with CRUD access via TxContext)
        context = call_registered_hooks(&lua, &event, context)?;

        Ok(context)
    }

    /// Run arbitrary hook refs with an active database connection injected.
    /// Used for system-level hooks like `on_init` that aren't tied to a collection.
    pub fn run_system_hooks_with_conn(
        &self,
        refs: &[String],
        conn: &dyn DbConnection,
    ) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        // Guard cleans up TxContext, UserContext, and UiLocaleContext on drop.
        let _guard = TxContextGuard::set(&lua, conn, None, None);

        for hook_ref in refs {
            tracing::debug!("Running system hook: {}", hook_ref);
            let ctx = HookContext::builder("", "init").build();
            call_hook_ref(&lua, hook_ref, ctx)?;
        }

        Ok(())
    }

    /// Run field-level hooks for a given event, mutating field values in-place.
    /// No CRUD/transaction access — use `run_field_hooks_with_conn` for before-write hooks.
    /// Each hook receives `(value, context)` and returns the new value.
    pub fn run_field_hooks(
        &self,
        fields: &[FieldDefinition],
        event: FieldHookEvent,
        data: &mut HashMap<String, Value>,
        collection: &str,
        operation: &str,
    ) -> Result<()> {
        // Skip VM acquisition if no fields have hooks for this event
        if !has_field_hooks_for_event(fields, &event) {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        run_field_hooks_inner(&lua, fields, &event, data, collection, operation)
    }

    /// Run field-level hooks with an active database connection/transaction injected.
    /// CRUD functions (`crap.collections.find`, `.create`, etc.) become available
    /// to Lua field hooks, sharing the provided connection for transaction atomicity.
    pub fn run_field_hooks_with_conn(
        &self,
        fields: &[FieldDefinition],
        event: FieldHookEvent,
        data: &mut HashMap<String, Value>,
        collection: &str,
        operation: &str,
        wctx: &FieldWriteCtx,
    ) -> Result<()> {
        // Skip VM acquisition if no fields have hooks for this event
        if !has_field_hooks_for_event(fields, &event) {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        // Inject the connection pointer so CRUD functions can use it.
        // Guard cleans up TxContext, UserContext, and UiLocaleContext on drop.
        let _guard = TxContextGuard::set(
            &lua,
            wctx.conn,
            wctx.user.cloned(),
            wctx.ui_locale.map(|s| s.to_string()),
        );

        run_field_hooks_inner(&lua, fields, &event, data, collection, operation)
    }
}
