//! Registration of `crap.collections.delete`, `update_many`, and `delete_many` Lua functions.

use std::collections::HashMap;

use anyhow::Result;
use mlua::Lua;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::{
        AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::{
        HookContext, HookEvent,
        lifecycle::{
            HookDepth, HookDepthGuard, MaxHookDepth, UiLocaleContext, UserContext,
            access::check_access_with_lua, converters::*, execution::run_hooks_inner,
        },
    },
    service::versions,
};

use super::get_tx_conn;

/// Register `crap.collections.delete(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_delete(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
) -> Result<()> {
    let reg = registry;
    let delete_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let hook_user = lua
                .app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let hook_ui_locale = lua
                .app_data_ref::<UiLocaleContext>()
                .and_then(|uc| uc.0.clone());

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(true);

            let run_hooks: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
                .unwrap_or(true);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_collection(&collection).cloned().ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("Collection '{}' not found", collection))
                })?
            };

            // Enforce access control when overrideAccess = false
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(
                    lua,
                    def.access.delete.as_deref(),
                    user_doc.as_ref(),
                    Some(&id),
                    None,
                )
                .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;

                if matches!(result, AccessResult::Denied) {
                    return Err(mlua::Error::RuntimeError("Delete access denied".into()));
                }
            }

            // Check hook depth for recursion protection
            let current_depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
            let max_depth = lua.app_data_ref::<MaxHookDepth>().map(|d| d.0).unwrap_or(3);
            let hooks_enabled = run_hooks && current_depth < max_depth;

            if run_hooks && current_depth >= max_depth {
                tracing::warn!(
                    "Hook depth {} reached max {}, skipping hooks for delete on {}",
                    current_depth,
                    max_depth,
                    collection
                );
            }

            let _depth_guard = if hooks_enabled {
                Some(HookDepthGuard::increment(lua, current_depth))
            } else {
                None
            };

            if hooks_enabled {
                let hook_ctx = HookContext::builder(&collection, "delete")
                    .data([("id".to_string(), Value::String(id.clone()))].into())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                run_hooks_inner(lua, &def.hooks, HookEvent::BeforeDelete, hook_ctx).map_err(
                    |e| mlua::Error::RuntimeError(format!("before_delete hook error: {}", e)),
                )?;
            }

            query::delete(conn, &collection, &id)
                .map_err(|e| mlua::Error::RuntimeError(format!("delete error: {}", e)))?;

            // Sync FTS index
            if conn.supports_fts() {
                query::fts::fts_delete(conn, &collection, &id)
                    .map_err(|e| mlua::Error::RuntimeError(format!("FTS delete error: {}", e)))?;
            }

            if hooks_enabled {
                let after_ctx = HookContext::builder(&collection, "delete")
                    .data([("id".to_string(), Value::String(id.clone()))].into())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                run_hooks_inner(lua, &def.hooks, HookEvent::AfterDelete, after_ctx).map_err(
                    |e| mlua::Error::RuntimeError(format!("after_delete hook error: {}", e)),
                )?;
            }

            Ok(true)
        },
    )?;
    table.set("delete", delete_fn)?;
    Ok(())
}

/// Register `crap.collections.update_many(collection, query, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_update_many(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let update_many_fn = lua.create_function(
        move |lua,
              (collection, query_table, data_table, opts): (
            String,
            mlua::Table,
            mlua::Table,
            Option<mlua::Table>,
        )| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(true);

            let run_hooks: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
                .unwrap_or(true);

            let hook_user = lua
                .app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let hook_ui_locale = lua
                .app_data_ref::<UiLocaleContext>()
                .and_then(|uc| uc.0.clone());

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_collection(&collection).cloned().ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("Collection '{}' not found", collection))
                })?
            };

            let draft: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("draft").ok().flatten())
                .unwrap_or(false);

            let (mut find_query, _) = lua_table_to_find_query(&query_table)?;
            normalize_filter_fields(&mut find_query.filters, &def.fields);

            if def.has_drafts() && !draft {
                find_query.filters.push(FilterClause::Single(Filter {
                    field: "_status".to_string(),
                    op: FilterOp::Equals("published".to_string()),
                }));
            }

            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(
                    lua,
                    def.access.read.as_deref(),
                    user_doc.as_ref(),
                    None,
                    None,
                )
                .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => {
                        return Err(mlua::Error::RuntimeError("Read access denied".into()));
                    }
                    AccessResult::Constrained(extra) => find_query.filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            let mut find_all = FindQuery::new();
            find_all.filters = find_query.filters;
            let docs = query::find(conn, &collection, &def, &find_all, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;

            // Check per-doc update access (all-or-nothing)
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                for doc in &docs {
                    let result = check_access_with_lua(
                        lua,
                        def.access.update.as_deref(),
                        user_doc.as_ref(),
                        Some(&doc.id),
                        None,
                    )
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;

                    if matches!(result, AccessResult::Denied) {
                        return Err(mlua::Error::RuntimeError(format!(
                            "Update access denied for document {}",
                            doc.id
                        )));
                    }
                }
            }

            // Hook depth check for recursion protection
            let current_depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
            let max_depth = lua.app_data_ref::<MaxHookDepth>().map(|d| d.0).unwrap_or(3);
            let hooks_enabled = run_hooks && current_depth < max_depth;

            if run_hooks && current_depth >= max_depth {
                tracing::warn!(
                    "Hook depth {} reached max {}, skipping hooks for update_many on {}",
                    current_depth,
                    max_depth,
                    collection
                );
            }

            let _depth_guard = if hooks_enabled {
                Some(HookDepthGuard::increment(lua, current_depth))
            } else {
                None
            };

            let data = lua_table_to_hashmap(&data_table)?;
            let join_data = lua_table_to_json_map(lua, &data_table)?;
            let mut modified = 0i64;

            for doc in &docs {
                if hooks_enabled {
                    let mut hook_data: HashMap<String, Value> = data
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect();
                    for (k, v) in &join_data {
                        hook_data.insert(k.clone(), v.clone());
                    }
                    let hook_ctx = HookContext::builder(&collection, "update")
                        .data(hook_data)
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::BeforeChange, hook_ctx).map_err(
                        |e| mlua::Error::RuntimeError(format!("before_change hook error: {}", e)),
                    )?;
                }

                let updated =
                    query::update(conn, &collection, &def, &doc.id, &data, locale_ctx.as_ref())
                        .map_err(|e| mlua::Error::RuntimeError(format!("update error: {}", e)))?;
                query::save_join_table_data(
                    conn,
                    &collection,
                    &def.fields,
                    &doc.id,
                    &join_data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| mlua::Error::RuntimeError(format!("join data error: {}", e)))?;
                if conn.supports_fts() {
                    query::fts::fts_upsert(conn, &collection, &updated, Some(&def)).map_err(
                        |e| mlua::Error::RuntimeError(format!("FTS upsert error: {}", e)),
                    )?;
                }

                if def.has_versions() {
                    let vs_ctx = versions::VersionSnapshotCtx::builder(&collection, &updated.id)
                        .fields(&def.fields)
                        .versions(def.versions.as_ref())
                        .has_drafts(def.has_drafts())
                        .build();
                    versions::create_version_snapshot(conn, &vs_ctx, "published", &updated)
                        .map_err(|e| {
                            mlua::Error::RuntimeError(format!("version snapshot error: {}", e))
                        })?;
                }

                if hooks_enabled {
                    let mut after_data = updated.fields.clone();
                    after_data.insert("id".to_string(), Value::String(updated.id.to_string()));
                    let after_ctx = HookContext::builder(&collection, "update")
                        .data(after_data)
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx).map_err(
                        |e| mlua::Error::RuntimeError(format!("after_change hook error: {}", e)),
                    )?;
                }

                modified += 1;
            }

            let result = lua.create_table()?;
            result.set("modified", modified)?;
            Ok(result)
        },
    )?;
    table.set("update_many", update_many_fn)?;
    Ok(())
}

/// Register `crap.collections.delete_many(collection, query, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_delete_many(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let delete_many_fn = lua.create_function(
        move |lua, (collection, query_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(true);

            let run_hooks: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
                .unwrap_or(true);

            let hook_user = lua
                .app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let hook_ui_locale = lua
                .app_data_ref::<UiLocaleContext>()
                .and_then(|uc| uc.0.clone());

            let locale_str: Option<String> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_collection(&collection).cloned().ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("Collection '{}' not found", collection))
                })?
            };

            let draft: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("draft").ok().flatten())
                .unwrap_or(false);

            let (mut find_query, _) = lua_table_to_find_query(&query_table)?;
            normalize_filter_fields(&mut find_query.filters, &def.fields);

            if def.has_drafts() && !draft {
                find_query.filters.push(FilterClause::Single(Filter {
                    field: "_status".to_string(),
                    op: FilterOp::Equals("published".to_string()),
                }));
            }

            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(
                    lua,
                    def.access.read.as_deref(),
                    user_doc.as_ref(),
                    None,
                    None,
                )
                .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => {
                        return Err(mlua::Error::RuntimeError("Read access denied".into()));
                    }
                    AccessResult::Constrained(extra) => find_query.filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            let mut find_all = FindQuery::new();
            find_all.filters = find_query.filters;
            let docs = query::find(conn, &collection, &def, &find_all, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;

            // Check per-doc delete access (all-or-nothing)
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                for doc in &docs {
                    let result = check_access_with_lua(
                        lua,
                        def.access.delete.as_deref(),
                        user_doc.as_ref(),
                        Some(&doc.id),
                        None,
                    )
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;

                    if matches!(result, AccessResult::Denied) {
                        return Err(mlua::Error::RuntimeError(format!(
                            "Delete access denied for document {}",
                            doc.id
                        )));
                    }
                }
            }

            // Hook depth check for recursion protection
            let current_depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
            let max_depth = lua.app_data_ref::<MaxHookDepth>().map(|d| d.0).unwrap_or(3);
            let hooks_enabled = run_hooks && current_depth < max_depth;

            if run_hooks && current_depth >= max_depth {
                tracing::warn!(
                    "Hook depth {} reached max {}, skipping hooks for delete_many on {}",
                    current_depth,
                    max_depth,
                    collection
                );
            }

            let _depth_guard = if hooks_enabled {
                Some(HookDepthGuard::increment(lua, current_depth))
            } else {
                None
            };

            let mut deleted = 0i64;
            for doc in &docs {
                if hooks_enabled {
                    let hook_ctx = HookContext::builder(&collection, "delete")
                        .data([("id".to_string(), Value::String(doc.id.to_string()))].into())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::BeforeDelete, hook_ctx).map_err(
                        |e| mlua::Error::RuntimeError(format!("before_delete hook error: {}", e)),
                    )?;
                }

                query::delete(conn, &collection, &doc.id)
                    .map_err(|e| mlua::Error::RuntimeError(format!("delete error: {}", e)))?;
                if conn.supports_fts() {
                    query::fts::fts_delete(conn, &collection, &doc.id).map_err(|e| {
                        mlua::Error::RuntimeError(format!("FTS delete error: {}", e))
                    })?;
                }

                if hooks_enabled {
                    let after_ctx = HookContext::builder(&collection, "delete")
                        .data([("id".to_string(), Value::String(doc.id.to_string()))].into())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::AfterDelete, after_ctx).map_err(
                        |e| mlua::Error::RuntimeError(format!("after_delete hook error: {}", e)),
                    )?;
                }

                deleted += 1;
            }

            let result = lua.create_table()?;
            result.set("deleted", deleted)?;
            Ok(result)
        },
    )?;
    table.set("delete_many", delete_many_fn)?;
    Ok(())
}
