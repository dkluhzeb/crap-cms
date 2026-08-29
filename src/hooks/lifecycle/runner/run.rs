//! `HookRunner` core run methods: collection hooks, field hooks, system hooks.

use anyhow::Result;

use crate::{
    core::{Builder, Document, DocumentFields, FieldDefinition, HookRef, collection::Hooks},
    db::DbConnection,
    hooks::{
        HookContext, HookEvent, HookRunner,
        lifecycle::{
            LuaCrudInfra,
            execution::{
                call_hook_ref, call_registered_hooks, get_hook_refs, has_field_hooks_for_event,
                run_field_hooks_inner,
            },
            types::{FieldHookEvent, TxContextGuard},
        },
    },
};

/// Bundled transaction context for field-level write hooks.
#[derive(Builder)]
pub struct FieldWriteCtx<'a> {
    #[builder(required)]
    pub conn: &'a dyn DbConnection,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
    /// Lua CRUD infrastructure to inject into the hook VM (cache, event
    /// transport, queues). `None` skips infra plumbing — used by tests and
    /// system hooks that don't publish events from inside the hook.
    pub infra: Option<LuaCrudInfra>,
}

/// Per-call descriptor for field-level hook execution.
///
/// Bundles the four "what to run" inputs that flow unchanged through the
/// hook stack: which fields, which event, and the collection + operation
/// labels used in `HookContext`. Threaded as a single `&FieldHooksCall`
/// instead of four positional args.
pub struct FieldHooksCall<'a> {
    pub fields: &'a [FieldDefinition],
    pub event: FieldHookEvent,
    pub collection: &'a str,
    pub operation: &'a str,
    /// The document id being processed (nil on create — no row yet).
    pub id: Option<&'a str>,
    /// Content locale for this operation (nil when not locale-scoped).
    pub locale: Option<&'a str>,
}

impl HookRunner {
    /// Run all hooks for a given event, mutating the context.
    /// Runs collection-level hook refs first, then global registered hooks.
    /// Does NOT provide CRUD access to hooks (use `run_hooks_with_conn` for that).
    ///
    /// # Errors
    ///
    /// Returns an error if a Lua VM cannot be acquired or any hook itself fails.
    pub fn run_hooks(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        mut context: HookContext,
    ) -> Result<HookContext> {
        let hook_refs = get_hook_refs(hooks, event);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for(event.as_str()) {
            return Ok(context);
        }

        let lua = self.pool.acquire()?;

        for hook_ref in hook_refs {
            tracing::debug!(
                "Running hook: {} for {}",
                hook_ref.reference(),
                context.collection
            );
            context = call_hook_ref(&lua, hook_ref, context)?;
        }

        // Run global registered hooks
        context = call_registered_hooks(&lua, event, context)?;

        Ok(context)
    }

    /// Run hooks with an active database connection/transaction injected.
    /// Runs collection-level hook refs first, then global registered hooks.
    /// CRUD functions (`crap.collections.find`, `.create`, etc.) become available
    /// to Lua hooks and share the provided connection for transaction atomicity.
    /// The authenticated user and UI locale are extracted from the `HookContext`.
    ///
    /// # Errors
    ///
    /// Returns an error if a Lua VM cannot be acquired or any hook itself fails.
    pub fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        mut context: HookContext,
        conn: &dyn DbConnection,
        infra: Option<LuaCrudInfra>,
    ) -> Result<HookContext> {
        let hook_refs = get_hook_refs(hooks, event);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for(event.as_str()) {
            return Ok(context);
        }

        let lua = self.pool.acquire()?;

        // Inject the connection pointer so CRUD functions can use it.
        // Safety: conn is valid for the duration of this method, and we hold
        // the Lua mutex so no concurrent access is possible.
        // Guard cleans up TxContext, UserContext, and UiLocaleContext on drop.
        let _guard = TxContextGuard::set(
            &lua,
            conn,
            context.user.clone(),
            context.ui_locale.clone(),
            infra,
        );

        for hook_ref in hook_refs {
            tracing::debug!(
                "Running hook (tx): {} for {}",
                hook_ref.reference(),
                context.collection
            );
            context = call_hook_ref(&lua, hook_ref, context)?;
        }

        // Run global registered hooks (with CRUD access via TxContext)
        context = call_registered_hooks(&lua, event, context)?;

        Ok(context)
    }

    /// Run arbitrary hook refs with an active database connection injected.
    /// Used for system-level hooks like `on_init` that aren't tied to a collection.
    ///
    /// # Errors
    ///
    /// Returns an error if a Lua VM cannot be acquired or any hook itself fails.
    pub fn run_system_hooks_with_conn(
        &self,
        refs: &[String],
        conn: &dyn DbConnection,
    ) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        // Guard cleans up TxContext, UserContext, UiLocaleContext, and LuaCrudInfra on drop.
        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        for hook_ref in refs {
            tracing::debug!("Running system hook: {}", hook_ref);
            let ctx = HookContext::builder("", "init").build();
            // System/init hooks are plain module refs with no per-config options.
            call_hook_ref(&lua, &HookRef::new(hook_ref.as_str()), ctx)?;
        }

        Ok(())
    }

    /// Run field-level hooks for a given event, mutating field values in-place.
    /// No CRUD/transaction access — use `run_field_hooks_with_conn` for before-write hooks.
    /// Each hook receives `(value, context)` and returns the new value.
    ///
    /// # Errors
    ///
    /// Returns an error if a Lua VM cannot be acquired or any field hook fails.
    pub fn run_field_hooks(
        &self,
        data: &mut DocumentFields,
        call: &FieldHooksCall<'_>,
    ) -> Result<()> {
        // Skip VM acquisition if no fields have hooks for this event
        if !has_field_hooks_for_event(call.fields, &call.event) {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        run_field_hooks_inner(&lua, data, call)
    }

    /// Run field-level hooks with an active database connection/transaction injected.
    /// CRUD functions (`crap.collections.find`, `.create`, etc.) become available
    /// to Lua field hooks, sharing the provided connection for transaction atomicity.
    ///
    /// # Errors
    ///
    /// Returns an error if a Lua VM cannot be acquired or any field hook fails.
    pub fn run_field_hooks_with_conn(
        &self,
        data: &mut DocumentFields,
        call: &FieldHooksCall<'_>,
        wctx: FieldWriteCtx<'_>,
    ) -> Result<()> {
        // Skip VM acquisition if no fields have hooks for this event
        if !has_field_hooks_for_event(call.fields, &call.event) {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        // Inject the connection pointer so CRUD functions can use it.
        // Guard cleans up TxContext, UserContext, UiLocaleContext, and LuaCrudInfra on drop.
        let _guard = TxContextGuard::set(
            &lua,
            wctx.conn,
            wctx.user.cloned(),
            wctx.ui_locale.map(std::string::ToString::to_string),
            wctx.infra,
        );

        run_field_hooks_inner(&lua, data, call)
    }
}
