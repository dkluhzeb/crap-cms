//! HookRunner core run methods: collection hooks, field hooks, system hooks.

use std::collections::HashMap;

use anyhow::Result;

use crate::core::collection::Hooks;
use crate::core::Document;
use crate::core::field::FieldDefinition;
use crate::hooks::lifecycle::context::HookContext;
use crate::hooks::lifecycle::execution::{
    get_hook_refs, has_field_hooks_for_event,
    call_registered_hooks, run_field_hooks_inner,
    call_hook_ref,
};
use crate::hooks::lifecycle::types::{
    TxContext, UserContext, UiLocaleContext, HookEvent, FieldHookEvent,
};

use super::HookRunner;

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
    /// `user` is the authenticated user (if any) — propagated to CRUD closures
    /// for `overrideAccess = false` enforcement.
    #[allow(clippy::too_many_arguments)]
    pub fn run_hooks_with_conn(
        &self,
        hooks: &Hooks,
        event: HookEvent,
        mut context: HookContext,
        conn: &rusqlite::Connection,
        user: Option<&Document>,
        ui_locale: Option<&str>,
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
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(user.cloned()));
        lua.set_app_data(UiLocaleContext(ui_locale.map(|s| s.to_string())));

        let result = (|| -> Result<HookContext> {
            for hook_ref in hook_refs {
                tracing::debug!("Running hook (tx): {} for {}", hook_ref, context.collection);
                context = call_hook_ref(&lua, hook_ref, context)?;
            }
            // Run global registered hooks (with CRUD access via TxContext)
            context = call_registered_hooks(&lua, &event, context)?;
            Ok(context)
        })();

        // Always clean up the connection pointer, even on error.
        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();
        lua.remove_app_data::<UiLocaleContext>();

        result
    }

    /// Run arbitrary hook refs with an active database connection injected.
    /// Used for system-level hooks like `on_init` that aren't tied to a collection.
    pub fn run_system_hooks_with_conn(
        &self,
        refs: &[String],
        conn: &rusqlite::Connection,
    ) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));
        lua.set_app_data(UiLocaleContext(None));

        let result = (|| -> Result<()> {
            for hook_ref in refs {
                tracing::debug!("Running system hook: {}", hook_ref);
                let ctx = HookContext::builder("", "init").build();
                call_hook_ref(&lua, hook_ref, ctx)?;
            }
            Ok(())
        })();

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();
        lua.remove_app_data::<UiLocaleContext>();

        result
    }

    /// Run field-level hooks for a given event, mutating field values in-place.
    /// No CRUD/transaction access — use `run_field_hooks_with_conn` for before-write hooks.
    /// Each hook receives `(value, context)` and returns the new value.
    pub fn run_field_hooks(
        &self,
        fields: &[FieldDefinition],
        event: FieldHookEvent,
        data: &mut HashMap<String, serde_json::Value>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn run_field_hooks_with_conn(
        &self,
        fields: &[FieldDefinition],
        event: FieldHookEvent,
        data: &mut HashMap<String, serde_json::Value>,
        collection: &str,
        operation: &str,
        conn: &rusqlite::Connection,
        user: Option<&Document>,
        ui_locale: Option<&str>,
    ) -> Result<()> {
        // Skip VM acquisition if no fields have hooks for this event
        if !has_field_hooks_for_event(fields, &event) {
            return Ok(());
        }

        let lua = self.pool.acquire()?;

        // Inject the connection pointer so CRUD functions can use it.
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(user.cloned()));
        lua.set_app_data(UiLocaleContext(ui_locale.map(|s| s.to_string())));

        let result = run_field_hooks_inner(&lua, fields, &event, data, collection, operation);

        // Always clean up, even on error.
        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();
        lua.remove_app_data::<UiLocaleContext>();

        result
    }
}
