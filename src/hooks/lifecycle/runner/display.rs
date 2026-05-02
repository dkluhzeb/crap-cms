//! HookRunner methods for display conditions and rendering.

use std::collections::HashMap;

use mlua::{Function, Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::{
    admin::custom_pages::CustomPage,
    hooks::{
        HookRunner, api,
        api::pages::PAGES_KEY,
        lifecycle::{
            execution::{
                call_display_condition_with_lua, has_registered_hooks, resolve_hook_function,
            },
            types::DisplayConditionResult,
        },
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

    /// Invoke a template-data function registered via
    /// `crap.template_data.register(name, fn)` from Lua. Returns the
    /// function's return value as JSON. None when no function is
    /// registered under `name`, or when the function errors.
    ///
    /// Called on demand by the `{{data "name"}}` Handlebars helper, so
    /// the function only runs on pages whose templates actually reference
    /// it.
    ///
    /// The function is invoked with the full page context as its single
    /// argument: `function(ctx) ... end`. Customers reach into
    /// `ctx.user`, `ctx.document`, `ctx.page`, `ctx.collection`, etc. to
    /// scope their data. Functions registered with no arguments still
    /// work — Lua silently drops the extra arg.
    pub fn call_template_data(&self, name: &str, page_ctx: &JsonValue) -> Option<JsonValue> {
        let lua = self.pool.acquire().ok()?;

        let table: Table = lua
            .named_registry_value(crate::hooks::api::template_data::TEMPLATE_DATA_KEY)
            .ok()?;
        let func: Function = match table.get(name) {
            Ok(f) => f,
            Err(_) => return None,
        };

        let ctx_lua = match api::json_to_lua(&lua, page_ctx) {
            Ok(v) => v,
            Err(e) => {
                warn!("crap.template_data['{name}']: failed to convert context to Lua: {e}");
                return None;
            }
        };

        match func.call::<Value>(ctx_lua) {
            Ok(v) => match api::lua_to_json(&lua, &v) {
                Ok(json) => Some(json),
                Err(e) => {
                    warn!("crap.template_data['{name}']: result is not JSON-encodable: {e}");
                    None
                }
            },
            Err(e) => {
                warn!("crap.template_data['{name}'] errored: {e}");
                None
            }
        }
    }

    /// Read every entry registered via `crap.pages.register(slug, opts)`
    /// and convert to the typed [`CustomPage`] list. Called once at
    /// admin-server startup to populate `AdminState.custom_pages`.
    ///
    /// Returns an empty Vec if Lua isn't available or no pages are
    /// registered.
    pub fn extract_custom_pages(&self) -> Vec<CustomPage> {
        let Ok(lua) = self.pool.acquire() else {
            return Vec::new();
        };

        let Ok(pages_table): LuaResult<Table> = lua.named_registry_value(PAGES_KEY) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for pair in pages_table.pairs::<String, Table>() {
            let Ok((slug, opts)) = pair else { continue };

            out.push(CustomPage {
                slug,
                section: opts.get::<String>("section").ok(),
                label: opts.get::<String>("label").ok(),
                icon: opts.get::<String>("icon").ok(),
                access: opts.get::<String>("access").ok(),
            });
        }

        out
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
