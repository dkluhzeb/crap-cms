//! Shared helper functions for CRUD Lua function registration.
//!
//! Extracts duplicated patterns from the registration closures (opts parsing,
//! user/locale extraction, registry lookup, hook depth checking, data extraction).

use std::sync::Arc;

use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};
use serde_json::Value;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, DocumentFields, GlobalDefinition, Registry,
        SharedInvalidationTransport,
    },
    hooks::lifecycle::{
        HookDepth, HookDepthGuard, LuaCrudInfra, LuaVmInfra, UiLocaleContext, UserContext,
        converters::{lua_table_to_hashmap, lua_table_to_json_map},
    },
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

/// Build a `LuaCrudInfra` from all available Lua `app_data` fields.
/// Returns `None` when no infra was threaded into the VM.
///
/// When this VM carries a per-VM cache handle (`[cache] backend = "custom"`,
/// see [`LuaVmInfra::cache`]), it shadows the caller-derived one so an in-VM
/// `clear_cache` runs on THIS VM's `LocalLease` instead of re-acquiring a
/// pool VM from inside a held one.
pub(crate) fn hook_lua_infra(lua: &Lua) -> Option<LuaCrudInfra> {
    let mut infra = lua.app_data_ref::<LuaCrudInfra>().map(|i| i.clone())?;

    if let Some(vm) = lua.app_data_ref::<LuaVmInfra>()
        && vm.cache.is_some()
    {
        infra.cache.clone_from(&vm.cache);
    }

    Some(infra)
}

/// Extract the invalidation transport from the VM-stable [`LuaVmInfra`]. Used
/// by delete operations to tear down live sessions for deleted
/// auth-collection users.
pub(crate) fn hook_invalidation_transport(lua: &Lua) -> Option<SharedInvalidationTransport> {
    lua.app_data_ref::<LuaVmInfra>()
        .and_then(|i| i.invalidation_transport.clone())
}

/// Extract the locale configuration from the VM-stable [`LuaVmInfra`]. Used by
/// write paths (notably `unpublish`) to thread `LocaleConfig` into a
/// `ServiceContext` so the service layer can build a default `LocaleContext`
/// for raw reads of localized fields.
pub(crate) fn hook_locale_config(lua: &Lua) -> Option<LocaleConfig> {
    lua.app_data_ref::<LuaVmInfra>()
        .map(|i| i.locale_config.clone())
}

/// Look up a collection definition from the registry snapshot, returning a
/// `RuntimeError` if not found.
pub(crate) fn resolve_collection(
    reg: &Registry,
    slug: &str,
) -> LuaResult<Arc<CollectionDefinition>> {
    reg.get_collection(slug)
        .cloned()
        .ok_or_else(|| RuntimeError(format!("Collection '{slug}' not found")))
}

/// Look up a global definition from the registry snapshot, returning a
/// `RuntimeError` if not found.
pub(crate) fn resolve_global(reg: &Registry, slug: &str) -> LuaResult<Arc<GlobalDefinition>> {
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
    let max_depth = lua
        .app_data_ref::<LuaVmInfra>()
        .map_or(3, |i| i.max_hook_depth);
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
        lua.set_app_data(LuaVmInfra {
            max_hook_depth: 3,
            ..Default::default()
        });

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
        lua.set_app_data(LuaVmInfra {
            max_hook_depth: 3,
            ..Default::default()
        });

        let (enabled, guard) = check_hook_depth(&lua, true, "test", "update_many");
        assert!(!enabled);
        assert!(guard.is_none());
    }

    #[test]
    fn check_hook_depth_disables_when_run_hooks_false() {
        let lua = Lua::new();
        lua.set_app_data(HookDepth(0));
        lua.set_app_data(LuaVmInfra {
            max_hook_depth: 3,
            ..Default::default()
        });

        let (enabled, guard) = check_hook_depth(&lua, false, "test", "delete");
        assert!(!enabled);
        assert!(guard.is_none());
    }
}
