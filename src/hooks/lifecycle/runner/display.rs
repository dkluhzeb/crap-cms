//! `HookRunner` methods for display conditions and rendering.

use std::sync::Arc;

use mlua::{Function, Lua, LuaSerdeExt as _, Result as LuaResult, Table, Value};
use serde::Serialize;
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::{
    admin::custom_pages::CustomPage,
    core::{Document, HookRef},
    db::DbPool,
    hooks::{
        HookRunner,
        lifecycle::{
            ConditionContext, TxContextGuard,
            execution::{
                call_display_condition_with_lua, has_registered_hooks, resolve_hook_function,
            },
            types::DisplayConditionResult,
        },
        lua_api,
        lua_api::pages::PAGES_KEY,
    },
    typegen::LuaAnnotation,
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
    ///
    /// `crud` is the same [`RenderCrud`] the page's `before_render` hook
    /// runs under, so the two render-time extension points have identical
    /// database access: read-only as the signed-in admin on authenticated
    /// pages, nothing at all on unauthenticated and error pages.
    pub fn call_template_data(
        &self,
        name: &str,
        page_ctx: &JsonValue,
        crud: &RenderCrud,
    ) -> Option<JsonValue> {
        // Logged, not swallowed: a VM-pool timeout makes the value vanish
        // from the page, and a widget that silently renders empty is very
        // hard to trace back to pool exhaustion. `run_before_render` warns
        // on the same failure.
        let lua = self
            .pool
            .acquire()
            .inspect_err(|e| warn!("crap.template_data['{name}']: VM pool error: {e}"))
            .ok()?;
        let _guard = crud.install(&lua);

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

    /// Run `before_render` hooks on an admin page's template context.
    ///
    /// Each hook is called as `fn(ctx, info)`:
    ///
    /// - `ctx` — the full template context as a Lua table. Tables are
    ///   pass-by-reference, so mutating `ctx` in place is enough; returning a
    ///   table replaces the context for the hooks that follow.
    /// - `info` — [`RenderInfo`]: which page is rendering, so a hook can
    ///   bail out in one line instead of guessing from which keys exist.
    ///
    /// [`RenderParams::crud`] decides whether the hooks get read-only
    /// database access (see [`RenderCrud`]). The whole context makes exactly
    /// ONE round trip through Lua no matter how many hooks are registered.
    ///
    /// Failures are always soft. A VM-pool or conversion failure logs a
    /// warning and returns the context untouched; a hook that raises is
    /// logged and skipped, keeping whatever the hooks around it did. A page
    /// is never failed by a hook.
    #[must_use]
    pub fn run_before_render(&self, params: RenderParams) -> JsonValue {
        let RenderParams {
            context,
            info,
            crud,
        } = params;

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

        // Identity is installed either way, so a read-only hook's CRUD runs
        // as the viewer (and an access-check hook sees the same `ctx.user`
        // every other hook does). Only the *database* half is conditional.
        // Installing it explicitly also guarantees a pooled VM cannot carry
        // a previous run's identity into this one.
        let _guard = crud.install(&lua);

        execute_render_hooks(&lua, context, &info)
    }
}

/// Which page is being rendered — the second argument handed to every
/// `before_render` hook.
///
/// Without this a hook has to infer the page from which context keys happen
/// to exist, which silently breaks whenever a page grows a field.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.template.render_info")]
pub struct RenderInfo {
    /// The page discriminant, matching `ctx.page.type`
    /// (`"dashboard"`, `"collection_items"`, `"error_404"`, …).
    pub page: String,

    /// The template being rendered, e.g. `"collections/items"`. Reflects
    /// the built-in name even when an overlay template has replaced it.
    pub template: String,

    /// Collection slug, on pages scoped to one collection.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub collection: Option<String>,

    /// Global slug, on pages scoped to one global.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub global: Option<String>,
}

impl RenderInfo {
    /// Derive the render info from the serialized page context plus the
    /// template name. Everything but the template name already lives in the
    /// context, so handlers do not have to restate it.
    #[must_use]
    pub fn from_context(template: &str, context: &JsonValue) -> Self {
        let slug_of = |key: &str| {
            context
                .get(key)
                .and_then(|v| v.get("slug"))
                .and_then(JsonValue::as_str)
                .map(str::to_string)
        };

        Self {
            page: context
                .get("page")
                .and_then(|p| p.get("type"))
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string(),
            template: template.to_string(),
            collection: slug_of("collection"),
            global: slug_of("global"),
        }
    }
}

/// What database access an admin render gets — shared by the
/// `before_render` hook and by `crap.template_data` functions, so the two
/// render-time extension points can never drift apart.
///
/// Cheap to clone (the viewer is behind an `Arc`) because a single page
/// render installs it once for the hook and again for every `{{data "…"}}`
/// site the template evaluates.
#[derive(Clone)]
pub enum RenderCrud {
    /// Read-only pool access — authenticated content pages. Reads draw from
    /// the read pool and run as `user`; every write op is refused (see
    /// [`PoolMode::ReadOnly`]).
    ///
    /// [`PoolMode::ReadOnly`]: crate::hooks::lifecycle::PoolMode
    ReadOnly {
        pool: DbPool,
        user: Option<Arc<Document>>,
        ui_locale: Option<String>,
    },
    /// No database access — unauthenticated pages (login, password reset,
    /// MFA) and error pages.
    ///
    /// This is a security boundary, not an optimization: with no signed-in
    /// user there is no identity to scope a query by, so any read a hook
    /// performed would either be denied or have to bypass access control to
    /// be useful — and its output would land on a page served to an
    /// anonymous visitor. Error pages additionally have to render when the
    /// database is the thing that failed.
    None {
        user: Option<Arc<Document>>,
        ui_locale: Option<String>,
    },
}

impl RenderCrud {
    /// No identity and no database — the safe default for any render that
    /// did not declare an access level.
    #[must_use]
    pub fn none() -> Self {
        Self::None {
            user: None,
            ui_locale: None,
        }
    }

    /// Install this access level on `lua` for the duration of one hook or
    /// template-data call. The returned guard restores the VM's previous
    /// context on drop.
    pub(crate) fn install<'a>(&self, lua: &'a Lua) -> TxContextGuard<'a> {
        let user = self.user().map(|u| (**u).clone());
        let ui_locale = self.ui_locale().map(str::to_string);

        match self {
            Self::ReadOnly { pool, .. } => {
                TxContextGuard::set_pool_read_only(lua, pool.clone(), user, ui_locale)
            }
            Self::None { .. } => TxContextGuard::set_identity(lua, user, ui_locale),
        }
    }

    fn user(&self) -> Option<&Arc<Document>> {
        match self {
            Self::ReadOnly { user, .. } | Self::None { user, .. } => user.as_ref(),
        }
    }

    fn ui_locale(&self) -> Option<&str> {
        match self {
            Self::ReadOnly { ui_locale, .. } | Self::None { ui_locale, .. } => ui_locale.as_deref(),
        }
    }
}

/// Arguments for [`HookRunner::run_before_render`].
pub struct RenderParams {
    /// The serialized page context handed to the template engine.
    pub context: JsonValue,
    /// Which page is rendering.
    pub info: RenderInfo,
    /// The database access the hooks get.
    pub crud: RenderCrud,
}

/// Execute all registered `before_render` hooks, piping one Lua table
/// through each in registration order.
///
/// The context is converted to Lua once and back once — Lua tables are
/// references, so a hook mutating `ctx` is observed by the next hook without
/// a round trip. A hook that *returns* a different table replaces the
/// current one for everything downstream.
fn execute_render_hooks(lua: &Lua, context: JsonValue, info: &RenderInfo) -> JsonValue {
    let hooks_table: Table = match lua
        .named_registry_value::<Table>("_crap_event_hooks")
        .and_then(|t| t.get::<Table>("before_render"))
    {
        Ok(t) => t,
        Err(_) => return context,
    };

    let mut current = match lua_api::json_to_lua(lua, &context) {
        Ok(Value::Table(t)) => t,
        Ok(_) => return context,
        Err(e) => {
            warn!("before_render: failed to convert context to Lua: {e}");

            return context;
        }
    };

    let info_lua = match lua.to_value(info) {
        Ok(v) => v,
        Err(e) => {
            warn!("before_render: failed to convert render info to Lua: {e}");

            return context;
        }
    };

    let len = hooks_table.raw_len();

    for i in 1..=len {
        let Ok(func) = hooks_table.raw_get::<Function>(i) else {
            continue;
        };

        match func.call::<Value>((&current, &info_lua)) {
            // A returned table replaces the context for later hooks.
            Ok(Value::Table(tbl)) => current = tbl,
            // `nil` means "I mutated ctx in place" (or did nothing).
            Ok(Value::Nil) => {}
            Ok(_) => {
                warn!("before_render hook returned non-table, non-nil value; ignoring");
            }
            Err(e) => {
                warn!("before_render hook error: {e}");
            }
        }
    }

    match lua_api::lua_to_json(&Value::Table(current)) {
        Ok(v) => v,
        Err(e) => {
            warn!("before_render: failed to convert Lua result to JSON: {e}");

            context
        }
    }
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

    // ── RenderInfo derivation ────────────────────────────────────────────

    #[test]
    fn render_info_reads_page_type_and_slugs_from_the_context() {
        let ctx = serde_json::json!({
            "page": { "type": "collection_items", "title": "Posts" },
            "collection": { "slug": "posts", "display_name": "Posts" },
        });

        let info = RenderInfo::from_context("collections/items", &ctx);

        assert_eq!(info.page, "collection_items");
        assert_eq!(info.template, "collections/items");
        assert_eq!(info.collection.as_deref(), Some("posts"));
        assert_eq!(info.global, None);
    }

    #[test]
    fn render_info_picks_up_a_global_slug() {
        let ctx = serde_json::json!({
            "page": { "type": "global_edit" },
            "global": { "slug": "settings" },
        });

        let info = RenderInfo::from_context("globals/edit", &ctx);

        assert_eq!(info.global.as_deref(), Some("settings"));
        assert_eq!(info.collection, None);
    }

    /// A context with no `page` block (or a malformed one) must still yield
    /// usable info rather than panicking — the template name alone is enough
    /// for a hook to branch on.
    #[test]
    fn render_info_tolerates_a_context_without_page_meta() {
        let info = RenderInfo::from_context("errors/500", &serde_json::json!({}));

        assert_eq!(info.page, "");
        assert_eq!(info.template, "errors/500");
        assert_eq!(info.collection, None);
        assert_eq!(info.global, None);
    }

    /// `collection` is only a slug carrier here — a context where it is not
    /// an object (or lacks `slug`) must not produce a bogus value.
    #[test]
    fn render_info_ignores_a_collection_field_that_is_not_a_slug_object() {
        let ctx = serde_json::json!({ "page": { "type": "dashboard" }, "collection": "posts" });

        assert_eq!(
            RenderInfo::from_context("dashboard/index", &ctx).collection,
            None
        );
    }
}
