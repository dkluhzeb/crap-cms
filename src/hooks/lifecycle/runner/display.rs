//! `HookRunner` methods for display conditions and rendering.

use mlua::{Function, Lua, LuaSerdeExt as _, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::{
    admin::custom_pages::CustomPage,
    core::HookRef,
    hooks::{
        HookRunner,
        lifecycle::{
            ConditionContext,
            execution::{
                call_display_condition_with_lua, has_registered_hooks, resolve_hook_function,
            },
            types::DisplayConditionResult,
        },
        lua_api,
        lua_api::pages::PAGES_KEY,
    },
};

impl HookRunner {
    /// Call a Lua function to compute a row label for an array/blocks row.
    /// Returns None if the function errors or returns nil.
    /// No CRUD access — pure formatting function.
    #[must_use]
    pub fn call_row_label(&self, func_ref: &str, row_data: &JsonValue) -> Option<String> {
        let lua = self.pool.acquire().ok()?;
        let func = resolve_hook_function(&lua, func_ref).ok()?;
        let row_lua = lua_api::json_to_lua(&lua, row_data).ok()?;

        match func.call::<Value>(row_lua) {
            Ok(Value::String(s)) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Evaluate a display condition function.
    /// Returns `DisplayConditionResult::Bool(visible)` or
    /// `DisplayConditionResult::Table { condition, visible }` depending on what Lua returns.
    /// No CRUD access — pure evaluation function.
    #[must_use]
    pub fn call_display_condition(
        &self,
        hook: &HookRef,
        form_data: &JsonValue,
        ctx: &ConditionContext<'_>,
    ) -> Option<DisplayConditionResult> {
        let lua = self.pool.acquire().ok()?;
        let cond_ctx = ConditionContext {
            options: hook.options(),
            ..*ctx
        };
        call_display_condition_with_lua(&lua, hook.reference(), form_data, &cond_ctx)
    }

    /// Evaluate display conditions for multiple fields using a single VM acquisition.
    /// Returns results **positionally aligned** with `conditions` — `result[i]`
    /// is for `conditions[i]`. Keyed by position rather than by ref string so two
    /// fields sharing a ref but differing in `options` get their own result (a
    /// ref string is no longer a unique key once `options` exist).
    #[must_use]
    pub fn call_display_conditions_batch(
        &self,
        conditions: &[(&HookRef, &JsonValue)],
        ctx: &ConditionContext<'_>,
    ) -> Vec<Option<DisplayConditionResult>> {
        if conditions.is_empty() {
            return Vec::new();
        }
        let Ok(lua) = self.pool.acquire() else {
            return vec![None; conditions.len()];
        };

        conditions
            .iter()
            .map(|&(hook, form_data)| {
                let cond_ctx = ConditionContext {
                    options: hook.options(),
                    ..*ctx
                };

                call_display_condition_with_lua(&lua, hook.reference(), form_data, &cond_ctx)
            })
            .collect()
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
            .named_registry_value(crate::hooks::lua_api::template_data::TEMPLATE_DATA_KEY)
            .ok()?;
        let func: Function = match table.get(name) {
            Ok(f) => f,
            Err(_) => return None,
        };

        let ctx_lua = match lua_api::json_to_lua(&lua, page_ctx) {
            Ok(v) => v,
            Err(e) => {
                warn!("crap.template_data['{name}']: failed to convert context to Lua: {e}");
                return None;
            }
        };

        match func.call::<Value>(ctx_lua) {
            Ok(v) => match lua_api::lua_to_json(&v) {
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
    #[must_use]
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

            if let Some(page) = page_from_entry(&lua, slug, &opts) {
                out.push(page);
            }
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

        let ctx_lua = match lua_api::json_to_lua(lua, &context) {
            Ok(v) => v,
            Err(e) => {
                warn!("before_render: failed to convert context to Lua: {e}");

                return context;
            }
        };

        match func.call::<Value>(ctx_lua) {
            Ok(Value::Table(tbl)) => match lua_api::lua_to_json(&Value::Table(tbl)) {
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

/// Convert one `slug → opts` page-registry entry into a [`CustomPage`].
///
/// `access` is a bare string or a `{ ref, options }` table — [`HookRef`]'s
/// serde shape decodes either via `lua.from_value`. `crap.pages.register` validates
/// the shape at load, so a decode failure here means a corrupted registry
/// entry — fail CLOSED by returning `None` (the page is dropped) rather than
/// serving the page without its access gate.
fn page_from_entry(lua: &Lua, slug: String, opts: &Table) -> Option<CustomPage> {
    let access = match opts.get::<Value>("access") {
        Ok(Value::Nil) => None,
        // An error reading the `access` key (e.g. a metatable `__index` that
        // raises) must fail CLOSED — drop the page rather than serve it
        // ungated, matching the decode-failure arm below.
        Err(e) => {
            warn!(
                "custom page '{slug}': error reading access value ({e}); \
                 dropping the page instead of serving it ungated"
            );
            return None;
        }
        Ok(v) => match lua.from_value::<HookRef>(v) {
            Ok(hook_ref) => Some(hook_ref),
            Err(e) => {
                warn!(
                    "custom page '{slug}': undecodable access value ({e}); \
                     dropping the page instead of serving it ungated"
                );
                return None;
            }
        },
    };

    Some(CustomPage {
        slug,
        section: opts.get::<String>("section").ok(),
        label: opts.get::<String>("label").ok(),
        icon: opts.get::<String>("icon").ok(),
        access,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(lua: &Lua) -> Table {
        lua.create_table().unwrap()
    }

    #[test]
    fn page_entry_decodes_bare_ref_and_options_table() {
        let lua = Lua::new();

        let bare = entry(&lua);
        bare.set("access", "hooks.access.admin").unwrap();
        let page = page_from_entry(&lua, "status".into(), &bare).unwrap();
        assert_eq!(
            page.access.as_ref().map(HookRef::reference),
            Some("hooks.access.admin")
        );

        let with_options = entry(&lua);
        let access = lua.create_table().unwrap();
        access.set("ref", "hooks.access.admin").unwrap();
        let options = lua.create_table().unwrap();
        options.set("role", "editor").unwrap();
        access.set("options", options).unwrap();
        with_options.set("access", access).unwrap();
        let page = page_from_entry(&lua, "reports".into(), &with_options).unwrap();
        assert!(page.access.unwrap().options().is_some());
    }

    #[test]
    fn page_without_access_has_no_gate() {
        let lua = Lua::new();
        let opts = entry(&lua);
        opts.set("label", "Status").unwrap();

        let page = page_from_entry(&lua, "status".into(), &opts).unwrap();
        assert!(page.access.is_none());
        assert_eq!(page.label.as_deref(), Some("Status"));
    }

    /// Regression: an undecodable `access` value must drop the page entirely
    /// (fail closed), never yield a page with `access: None` (fail open —
    /// the page would be served publicly without its configured gate).
    #[test]
    fn corrupted_access_value_drops_the_page() {
        let lua = Lua::new();
        let opts = entry(&lua);
        opts.set("access", 42).unwrap();

        assert!(
            page_from_entry(&lua, "status".into(), &opts).is_none(),
            "a page whose access gate can't be decoded must not be served"
        );
    }
}
