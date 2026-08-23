//! Shared helper functions for CRUD Lua function registration.
//!
//! Extracts duplicated patterns from the registration closures (opts parsing,
//! user/locale extraction, registry lookup, hook depth checking, data extraction).

use crate::hooks::lua_api::utils::lua_err;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};
use serde_json::Value;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, DocumentFields, GlobalDefinition, HookRef, Registry,
        SharedInvalidationTransport,
    },
    db::{AccessResult, FilterClause, query::SharedPopulateSingleflight},
    hooks::lifecycle::{
        AccessCheckInput, HookDepth, HookDepthGuard, LuaCrudInfra, LuaInvalidationTransport,
        LuaLocaleConfig, LuaPopulateSingleflight, MaxHookDepth, UiLocaleContext, UserContext,
        access::check_access_with_lua,
        converters::{lua_table_to_hashmap, lua_table_to_json_map},
    },
    service::{validate_access_constraint_locales, validate_access_constraints},
};

/// Extract the authenticated user document from Lua `app_data` (if present).
pub(crate) fn hook_user(lua: &Lua) -> Option<Document> {
    lua.app_data_ref::<UserContext>()
        .and_then(|uc| uc.0.clone())
}

/// Extract the UI locale string from Lua `app_data` (if present).
pub(crate) fn hook_ui_locale(lua: &Lua) -> Option<String> {
    lua.app_data_ref::<UiLocaleContext>()
        .and_then(|uc| uc.0.clone())
}

/// Extract the process-wide populate singleflight from Lua `app_data` (if set
/// via `HookRunner::builder().populate_singleflight(..)`). Returns `None` when
/// no singleflight was threaded in, so the service layer falls back to a
/// fresh per-call singleflight. For override-access reads the service layer
/// discards this Arc via its access-leak guardrail.
pub(crate) fn hook_populate_singleflight(lua: &Lua) -> Option<SharedPopulateSingleflight> {
    lua.app_data_ref::<LuaPopulateSingleflight>()
        .map(|sf| sf.0.clone())
}

/// Build a `LuaCrudInfra` from all available Lua `app_data` fields.
/// Returns `None` when no infra was threaded into the VM.
pub(crate) fn hook_lua_infra(lua: &Lua) -> Option<LuaCrudInfra> {
    lua.app_data_ref::<LuaCrudInfra>().map(|i| i.clone())
}

/// Extract the invalidation transport from Lua `app_data` (if set via
/// `HookRunner::builder().invalidation_transport(..)`). Used by delete
/// operations to tear down live sessions for deleted auth-collection users.
pub(crate) fn hook_invalidation_transport(lua: &Lua) -> Option<SharedInvalidationTransport> {
    lua.app_data_ref::<LuaInvalidationTransport>()
        .map(|t| t.0.clone())
}

/// Extract the locale configuration from Lua `app_data` (set during VM
/// initialization). Used by write paths (notably `unpublish`) to thread
/// `LocaleConfig` into a `ServiceContext` so the service layer can build
/// a default `LocaleContext` for raw reads of localized fields.
pub(crate) fn hook_locale_config(lua: &Lua) -> Option<LocaleConfig> {
    lua.app_data_ref::<LuaLocaleConfig>().map(|lc| lc.0.clone())
}

/// Look up a collection definition from the registry snapshot, returning a
/// `RuntimeError` if not found.
pub(crate) fn resolve_collection(reg: &Registry, slug: &str) -> LuaResult<CollectionDefinition> {
    reg.get_collection(slug)
        .cloned()
        .ok_or_else(|| RuntimeError(format!("Collection '{slug}' not found")))
}

/// Look up a global definition from the registry snapshot, returning a
/// `RuntimeError` if not found.
pub(crate) fn resolve_global(reg: &Registry, slug: &str) -> LuaResult<GlobalDefinition> {
    reg.get_global(slug)
        .cloned()
        .ok_or_else(|| RuntimeError(format!("Global '{slug}' not found")))
}

/// Check hook recursion depth and return whether hooks are enabled plus an
/// optional RAII guard that restores the depth on drop.
///
/// When `run_hooks` is false, hooks are unconditionally disabled.
/// When the current depth has reached `max_depth`, a warning is logged and
/// hooks are disabled for this call.
pub(crate) fn check_hook_depth<'a>(
    lua: &'a Lua,
    run_hooks: bool,
    collection: &str,
    operation: &str,
) -> (bool, Option<HookDepthGuard<'a>>) {
    let current_depth = lua.app_data_ref::<HookDepth>().map_or(0, |d| d.0);
    let max_depth = lua.app_data_ref::<MaxHookDepth>().map_or(3, |d| d.0);
    let hooks_enabled = run_hooks && current_depth < max_depth;

    if run_hooks && current_depth >= max_depth {
        warn!(
            "Hook depth {} reached max {}, skipping hooks for {} on {}",
            current_depth, max_depth, operation, collection
        );
    }

    let guard = if hooks_enabled {
        Some(HookDepthGuard::increment(lua, current_depth))
    } else {
        None
    };

    (hooks_enabled, guard)
}

/// Parameters for [`enforce_access`].
pub(crate) struct EnforceAccessParams<'a> {
    pub slug: &'a str,
    pub override_access: bool,
    pub access_fn: Option<&'a HookRef>,
    pub id: Option<&'a str>,
    /// The incoming write data, surfaced to the access fn as `ctx.data` —
    /// `update_many` passes the patch so data-gating access fns work on the
    /// bulk pre-flight exactly like on the per-document check; `delete_many`
    /// has no incoming data and passes `None`.
    pub data: Option<&'a DocumentFields>,
    pub deny_msg: &'a str,
    /// The operation reported to the access function as `ctx.operation`
    /// (e.g. `"update"` for `update_many`, `"delete"` for `delete_many`).
    /// The access fn passed in is that operation's gate, so the operation
    /// label must match it — not the `"find"` used to select candidate rows.
    pub operation: &'a str,
    /// Whether the caller is about to inject a `_status = 'published'` filter.
    /// Controls whether access-hook `_status` constraints are accepted.
    pub injecting_status: bool,
}

/// Enforce access control: check the given access function, merge constrained filters, deny if blocked.
///
/// Returns `Ok(())` if access is allowed (possibly after extending `filters` with constraints).
/// Returns `Err` with a `RuntimeError` if access is denied or if an access hook
/// returns a filter table that references a disallowed system column.
/// When `override_access` is true, skips the check entirely.
pub(crate) fn enforce_access(
    lua: &Lua,
    params: &EnforceAccessParams<'_>,
    filters: &mut Vec<FilterClause>,
) -> LuaResult<()> {
    if params.override_access {
        return Ok(());
    }

    let user_doc = hook_user(lua);
    let result = check_access_with_lua(
        lua,
        &AccessCheckInput::builder(params.operation, params.slug)
            .access(params.access_fn)
            .user(user_doc.as_ref())
            .id(params.id)
            .data(params.data)
            .build(),
    )
    .map_err(|e| RuntimeError(format!("access check error: {e:#}")))?;

    match result {
        AccessResult::Denied => Err(RuntimeError(params.deny_msg.to_string())),
        AccessResult::Constrained(extra) => {
            validate_access_constraints(&extra, false, params.injecting_status, params.slug)
                .map_err(lua_err)?;

            // Reject locale-scoped-field constraints here too (Lua bulk
            // update_many/delete_many): consume them as SQL only, so no leak,
            // but keep operator feedback uniform with every other surface. The
            // registry snapshot is in the VM's app-data.
            if let Some(registry) = lua.app_data_ref::<std::sync::Arc<crate::core::Registry>>() {
                let fields = registry
                    .get_collection(params.slug)
                    .map(|d| &d.fields)
                    .or_else(|| registry.get_global(params.slug).map(|d| &d.fields));

                if let Some(fields) = fields {
                    validate_access_constraint_locales(&extra, fields, params.slug)
                        .map_err(lua_err)?;
                }
            }

            filters.extend(extra);
            Ok(())
        }
        AccessResult::Allowed => Ok(()),
    }
}

/// Extracted data from a Lua data table for create/update/validate operations.
///
/// `data` is the merged typed-pipeline view: scalar columns (with group fields
/// flattened to `parent__child` keys) plus relations / has-many / arrays /
/// blocks. The service layer routes each entry to a column write or join-table
/// write based on the field's type. `password` is split off for auth
/// collections so it can be hashed before insert.
pub(crate) struct ExtractedData {
    pub(crate) data: DocumentFields,
    pub(crate) password: Option<String>,
}

/// Extract a merged typed data map and password from a Lua data table.
///
/// Shared by `create`, `update`, and `validate`: builds the document data map
/// and separates the password for auth collections. The Lua table yields two
/// views — `lua_table_to_hashmap` stringifies scalar leaves, while
/// `lua_table_to_json_map` preserves typed/nested shapes (arrays, blocks, and
/// **group objects**) — so the merged map starts with the stringified scalars
/// (wrapped via `values_from_strings`) and then overrides composite leaves with
/// their typed counterparts. Group objects stay nested (the canonical shape);
/// the service write entry re-normalizes and the DB edge flattens to columns.
pub(crate) fn extract_data(
    data_table: &Table,
    def: &CollectionDefinition,
) -> LuaResult<ExtractedData> {
    let mut flat = lua_table_to_hashmap(data_table)?;

    let password = if def.is_auth_collection() {
        flat.remove("password")
    } else {
        None
    };

    let mut data = crate::service::values_from_strings(flat);

    let composite_data: DocumentFields = lua_table_to_json_map(data_table)?
        .into_iter()
        .filter(|(_, v)| !matches!(v, Value::String(_)))
        .collect();
    data.extend(composite_data);

    if def.is_auth_collection() {
        data.remove("password");
    }

    Ok(ExtractedData { data, password })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn hook_user_returns_none_without_context() {
        let lua = Lua::new();
        assert!(hook_user(&lua).is_none());
    }

    #[test]
    fn hook_ui_locale_returns_none_without_context() {
        let lua = Lua::new();
        assert!(hook_ui_locale(&lua).is_none());
    }

    #[test]
    fn check_hook_depth_enables_hooks_when_under_limit() {
        let lua = Lua::new();
        lua.set_app_data(HookDepth(0));
        lua.set_app_data(MaxHookDepth(3));

        let (enabled, guard) = check_hook_depth(&lua, true, "test", "delete");
        assert!(enabled);
        assert!(guard.is_some());

        // Depth should be incremented
        assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 1);

        // Guard drop restores depth
        drop(guard);
        assert_eq!(lua.app_data_ref::<HookDepth>().unwrap().0, 0);
    }

    #[test]
    fn check_hook_depth_disables_when_at_limit() {
        let lua = Lua::new();
        lua.set_app_data(HookDepth(3));
        lua.set_app_data(MaxHookDepth(3));

        let (enabled, guard) = check_hook_depth(&lua, true, "test", "update_many");
        assert!(!enabled);
        assert!(guard.is_none());
    }

    #[test]
    fn check_hook_depth_disables_when_run_hooks_false() {
        let lua = Lua::new();
        lua.set_app_data(HookDepth(0));
        lua.set_app_data(MaxHookDepth(3));

        let (enabled, guard) = check_hook_depth(&lua, false, "test", "delete");
        assert!(!enabled);
        assert!(guard.is_none());
    }
}
