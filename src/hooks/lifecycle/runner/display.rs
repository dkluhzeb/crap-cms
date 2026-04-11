//! HookRunner methods for display conditions and rendering.

use std::collections::HashMap;

use mlua::{Lua, Table, Value};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::hooks::{
    HookRunner, api,
    lifecycle::{
        execution::{call_display_condition_with_lua, has_registered_hooks, resolve_hook_function},
        types::DisplayConditionResult,
    },
};

impl HookRunner {
    /// Call a Lua function to compute a row label for an array/blocks row.
    /// Returns None if the function errors or returns nil.
    /// No CRUD access — pure formatting function.
    pub fn call_row_label(&self, func_ref: &str, row_data: &JsonValue) -> Option<String> {
        let lua = self.pool.acquire().ok()?;
        let func = resolve_hook_function(&lua, func_ref).ok()?;
        let row_lua = api::json_to_lua(&lua, row_data).ok()?;

        match func.call::<Value>(row_lua) {
            Ok(Value::String(s)) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Evaluate a display condition function.
    /// Returns `DisplayConditionResult::Bool(visible)` or
    /// `DisplayConditionResult::Table { condition, visible }` depending on what Lua returns.
    /// No CRUD access — pure evaluation function.
    pub fn call_display_condition(
        &self,
        func_ref: &str,
        form_data: &JsonValue,
    ) -> Option<DisplayConditionResult> {
        let lua = self.pool.acquire().ok()?;
        call_display_condition_with_lua(&lua, func_ref, form_data)
    }

    /// Evaluate display conditions for multiple fields using a single VM acquisition.
    /// Returns a map from func_ref to the evaluation result.
    pub fn call_display_conditions_batch(
        &self,
        conditions: &[(&str, &JsonValue)],
    ) -> HashMap<String, DisplayConditionResult> {
        if conditions.is_empty() {
            return HashMap::new();
        }
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(_) => return HashMap::new(),
        };
        let mut results = HashMap::new();

        for &(func_ref, form_data) in conditions {
            if let Some(result) = call_display_condition_with_lua(&lua, func_ref, form_data) {
                results.insert(func_ref.to_string(), result);
            }
        }

        results
    }

    /// Run `before_render` hooks on the template context.
    /// Global registered `before_render` hooks receive the full template context as a
    /// Lua table and return the (potentially modified) context. No CRUD access.
    /// On error: logs warning, returns original context unmodified.
    pub fn run_before_render(&self, context: JsonValue) -> JsonValue {
        if !self.has_registered_hooks_for("before_render") {
            return context;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                warn!("VM pool error in run_before_render: {e}");

                return context;
            }
        };

        if !has_registered_hooks(&lua, "before_render") {
            return context;
        }

        execute_render_hooks(&lua, context)
    }
}

/// Execute all registered `before_render` hooks, piping context through each.
fn execute_render_hooks(lua: &Lua, mut context: JsonValue) -> JsonValue {
    let hooks_table: Table = match lua
        .named_registry_value::<Table>("_crap_event_hooks")
        .and_then(|t| t.get::<Table>("before_render"))
    {
        Ok(t) => t,
        Err(_) => return context,
    };

    let len = hooks_table.raw_len();

    for i in 1..=len {
        let func: mlua::Function = match hooks_table.raw_get(i) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let ctx_lua = match api::json_to_lua(lua, &context) {
            Ok(v) => v,
            Err(e) => {
                warn!("before_render: failed to convert context to Lua: {e}");

                return context;
            }
        };

        match func.call::<Value>(ctx_lua) {
            Ok(Value::Table(tbl)) => match api::lua_to_json(lua, &Value::Table(tbl)) {
                Ok(new_ctx) => context = new_ctx,
                Err(e) => {
                    warn!("before_render: failed to convert Lua result to JSON: {e}");
                }
            },
            Ok(Value::Nil) => {}
            Ok(_) => {
                warn!("before_render hook returned non-table, non-nil value; ignoring");
            }
            Err(e) => {
                warn!("before_render hook error: {e}");
            }
        }
    }

    context
}
