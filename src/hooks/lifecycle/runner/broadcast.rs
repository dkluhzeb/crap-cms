//! HookRunner methods for event broadcasting.

use std::collections::HashMap;

use anyhow::Result;
use mlua::Value;

use crate::core::collection::{CollectionHooks, LiveSetting};
use crate::core::event::{EventBus, EventOperation, EventTarget, EventUser};
use crate::hooks::lifecycle::context::HookContext;
use crate::hooks::lifecycle::execution::{
    call_before_broadcast_hook, call_registered_before_broadcast, get_hook_refs,
    resolve_hook_function,
};
use crate::hooks::lifecycle::types::HookEvent;

use super::HookRunner;

impl HookRunner {
    /// Run before_broadcast hooks. Returns Ok(Some(data)) to broadcast (possibly
    /// with transformed data), or Ok(None) to suppress the event.
    /// No CRUD access (fires after commit, same as after_change).
    pub fn run_before_broadcast(
        &self,
        hooks: &CollectionHooks,
        collection: &str,
        operation: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Option<HashMap<String, serde_json::Value>>> {
        let hook_refs = get_hook_refs(hooks, &HookEvent::BeforeBroadcast);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for("before_broadcast") {
            return Ok(Some(data));
        }

        let ctx = HookContext::builder(collection, operation)
            .data(data)
            .build();

        let lua = self.pool.acquire()?;

        let mut context = ctx;

        // Run collection-level hook refs first
        for hook_ref in hook_refs {
            tracing::debug!(
                "Running before_broadcast hook: {} for {}",
                hook_ref,
                context.collection
            );
            match call_before_broadcast_hook(&lua, hook_ref, context.clone())? {
                Some(new_ctx) => context = new_ctx,
                None => return Ok(None), // suppressed
            }
        }

        // Run global registered hooks
        match call_registered_before_broadcast(&lua, context)? {
            Some(ctx) => Ok(Some(ctx.data)),
            None => Ok(None),
        }
    }

    /// Check if a live event should be broadcast for this mutation.
    /// Returns Ok(true) to broadcast, Ok(false) to suppress.
    /// Runs WITHOUT transaction access (after write committed).
    pub fn check_live_setting(
        &self,
        live: Option<&LiveSetting>,
        collection: &str,
        operation: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<bool> {
        match live {
            None => Ok(true), // absent = broadcast all
            Some(LiveSetting::Disabled) => Ok(false),
            Some(LiveSetting::Function(func_ref)) => {
                let lua = self.pool.acquire()?;

                let func = resolve_hook_function(&lua, func_ref)?;

                let ctx_table = lua.create_table()?;
                ctx_table.set("collection", collection)?;
                ctx_table.set("operation", operation)?;
                let data_table = lua.create_table()?;
                for (k, v) in data {
                    data_table.set(k.as_str(), crate::hooks::api::json_to_lua(&lua, v)?)?;
                }
                ctx_table.set("data", data_table)?;

                let result: Value = func.call(ctx_table)?;
                match result {
                    Value::Boolean(b) => Ok(b),
                    Value::Nil => Ok(false),
                    _ => Ok(true),
                }
            }
        }
    }

    /// Publish a mutation event: check live setting → run before_broadcast hooks → EventBus.publish().
    /// Spawns into a background task (non-blocking, like fire_after_event).
    /// Untestable: spawns tokio::task::spawn_blocking for async event dispatch.
    #[allow(clippy::too_many_arguments)]
    #[cfg(not(tarpaulin_include))]
    pub fn publish_event(
        &self,
        event_bus: &Option<EventBus>,
        hooks: &CollectionHooks,
        live_setting: Option<&LiveSetting>,
        target: EventTarget,
        operation: EventOperation,
        collection: String,
        document_id: String,
        data: HashMap<String, serde_json::Value>,
        edited_by: Option<EventUser>,
    ) {
        let bus = match event_bus {
            Some(b) => b.clone(),
            None => return,
        };

        let runner = self.clone();
        let hooks = hooks.clone();
        let live = live_setting.cloned();
        let op_str = match &operation {
            EventOperation::Create => "create",
            EventOperation::Update => "update",
            EventOperation::Delete => "delete",
        }
        .to_string();

        tokio::task::spawn_blocking(move || {
            // 1. Check live setting
            match runner.check_live_setting(live.as_ref(), &collection, &op_str, &data) {
                Ok(false) => return,
                Err(e) => {
                    tracing::warn!("live setting check error for {}: {}", collection, e);
                    return;
                }
                Ok(true) => {}
            }

            // 2. Run before_broadcast hooks
            let broadcast_data =
                match runner.run_before_broadcast(&hooks, &collection, &op_str, data) {
                    Ok(Some(d)) => d,
                    Ok(None) => return, // suppressed
                    Err(e) => {
                        tracing::warn!("before_broadcast hook error for {}: {}", collection, e);
                        return;
                    }
                };

            // 3. Publish to EventBus
            bus.publish(
                target,
                operation,
                collection,
                document_id,
                broadcast_data,
                edited_by,
            );
        });
    }
}
