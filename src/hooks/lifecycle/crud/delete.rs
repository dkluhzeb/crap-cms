//! Registration of `crap.collections.delete`, `restore`, `update_many`, and `delete_many` Lua functions.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{SharedRegistry, upload},
    db::{
        AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::{
        HookContext, HookEvent, ValidationCtx,
        lifecycle::{
            ConfigDir, FieldHookEvent, HookDepth, HookDepthGuard, MaxHookDepth, UiLocaleContext,
            UserContext,
            access::{check_access_with_lua, check_field_write_access_with_lua},
            converters::*,
            execution::{run_field_hooks_inner, run_hooks_inner},
            validation::validate_fields_inner,
        },
    },
    service::versions,
};

use super::get_tx_conn;

/// Register `crap.collections.delete(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_delete(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let delete_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
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
                .unwrap_or(false);

            let run_hooks: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
                .unwrap_or(true);

            let force_hard_delete: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("forceHardDelete").ok().flatten())
                .unwrap_or(false);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| RuntimeError(format!("Collection '{}' not found", collection)))?
            };

            // Enforce access control when overrideAccess = false
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());

                // Soft delete uses trash permission (fallback to update);
                // hard delete uses delete permission
                let access_ref = if def.soft_delete && !force_hard_delete {
                    def.access.resolve_trash()
                } else {
                    def.access.delete.as_deref()
                };

                let result =
                    check_access_with_lua(lua, access_ref, user_doc.as_ref(), Some(&id), None)
                        .map_err(|e| RuntimeError(format!("access check error: {}", e)))?;

                if matches!(result, AccessResult::Denied) {
                    return Err(RuntimeError("Delete access denied".into()));
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

            // Block deletion of documents that are referenced by other documents.
            if !force_hard_delete {
                let ref_count = query::ref_count::get_ref_count(conn, &collection, &id)
                    .map_err(|e| RuntimeError(format!("ref count check error: {}", e)))?;
                if ref_count > 0 {
                    return Err(RuntimeError(format!(
                        "Cannot delete '{}' from '{}': referenced by {} document(s)",
                        id, collection, ref_count
                    )));
                }
            }

            // For upload collections, load the document before deleting to get file paths
            let upload_doc_fields = if def.is_upload_collection() {
                query::find_by_id(conn, &collection, &def, &id, None)
                    .ok()
                    .flatten()
                    .map(|doc| doc.fields.clone())
            } else {
                None
            };

            if hooks_enabled {
                let hook_ctx = HookContext::builder(&collection, "delete")
                    .data([("id".to_string(), Value::String(id.clone()))].into())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                run_hooks_inner(lua, &def.hooks, HookEvent::BeforeDelete, hook_ctx)
                    .map_err(|e| RuntimeError(format!("before_delete hook error: {}", e)))?;
            }

            // Decrement ref counts before hard delete (CASCADE removes junction rows).
            if !def.soft_delete || force_hard_delete {
                query::ref_count::before_hard_delete(conn, &collection, &id, &def.fields, &lc)
                    .map_err(|e| RuntimeError(format!("ref count error: {}", e)))?;
            }

            if def.soft_delete && !force_hard_delete {
                let deleted = query::soft_delete(conn, &collection, &id)
                    .map_err(|e| RuntimeError(format!("soft_delete error: {}", e)))?;
                if !deleted {
                    return Err(RuntimeError(format!(
                        "Document '{}' not found or already deleted in '{}'",
                        id, collection
                    )));
                }
            } else {
                query::delete(conn, &collection, &id)
                    .map_err(|e| RuntimeError(format!("delete error: {}", e)))?;
            }

            // Sync FTS index (remove from FTS for both hard and soft delete)
            if conn.supports_fts() {
                query::fts::fts_delete(conn, &collection, &id)
                    .map_err(|e| RuntimeError(format!("FTS delete error: {}", e)))?;
            }

            // Clean up upload files after successful DB delete (skip for soft-delete)
            if !def.soft_delete
                && let Some(fields) = upload_doc_fields
                && let Some(config_dir) = lua.app_data_ref::<ConfigDir>()
            {
                upload::delete_upload_files(&config_dir.0, &fields);
            }

            if hooks_enabled {
                let after_ctx = HookContext::builder(&collection, "delete")
                    .data([("id".to_string(), Value::String(id.clone()))].into())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                run_hooks_inner(lua, &def.hooks, HookEvent::AfterDelete, after_ctx)
                    .map_err(|e| RuntimeError(format!("after_delete hook error: {}", e)))?;
            }

            Ok(true)
        },
    )?;
    table.set("delete", delete_fn)?;
    Ok(())
}

/// Register `crap.collections.restore(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_restore(lua: &Lua, table: &Table, registry: SharedRegistry) -> Result<()> {
    let reg = registry;
    let restore_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(false);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| RuntimeError(format!("Collection '{}' not found", collection)))?
            };

            if !def.soft_delete {
                return Err(RuntimeError(format!(
                    "Collection '{}' does not have soft_delete enabled",
                    collection
                )));
            }

            // Check trash access (restore is the inverse of soft-delete)
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(
                    lua,
                    def.access.resolve_trash(),
                    user_doc.as_ref(),
                    Some(&id),
                    None,
                )
                .map_err(|e| RuntimeError(format!("access check error: {}", e)))?;

                if matches!(result, AccessResult::Denied) {
                    return Err(RuntimeError("Restore access denied".into()));
                }
            }

            let restored = query::restore(conn, &collection, &id)
                .map_err(|e| RuntimeError(format!("restore error: {}", e)))?;

            if !restored {
                return Err(RuntimeError(format!(
                    "Document '{}' not found or not deleted in '{}'",
                    id, collection
                )));
            }

            // Re-sync FTS index
            if conn.supports_fts()
                && let Ok(Some(doc)) =
                    query::find_by_id_unfiltered(conn, &collection, &def, &id, None)
            {
                query::fts::fts_upsert(conn, &collection, &doc, Some(&def))
                    .map_err(|e| RuntimeError(format!("FTS upsert error: {}", e)))?;
            }

            Ok(true)
        },
    )?;
    table.set("restore", restore_fn)?;
    Ok(())
}

/// Register `crap.collections.update_many(collection, query, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_update_many(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let update_many_fn = lua.create_function(
        move |lua,
              (collection, query_table, data_table, opts): (
            String,
            Table,
            Table,
            Option<Table>,
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
                .unwrap_or(false);

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
                    .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| RuntimeError(format!("Collection '{}' not found", collection)))?
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
                    def.access.update.as_deref(),
                    user_doc.as_ref(),
                    None,
                    None,
                )
                .map_err(|e| RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => {
                        return Err(RuntimeError("Update access denied".into()));
                    }
                    AccessResult::Constrained(extra) => find_query.filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            let mut find_all = FindQuery::new();
            find_all.filters = find_query.filters;
            let docs = query::find(conn, &collection, &def, &find_all, locale_ctx.as_ref())
                .map_err(|e| RuntimeError(format!("find error: {}", e)))?;

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
                    .map_err(|e| RuntimeError(format!("access check error: {}", e)))?;

                    if matches!(result, AccessResult::Denied) {
                        return Err(RuntimeError(format!(
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

            let mut data = lua_table_to_hashmap(&data_table)?;

            // Reject password field in update_many for auth collections.
            // Bulk password changes are not supported — use single update instead.
            if def.is_auth_collection() && data.contains_key("password") {
                return Err(RuntimeError(
                    "Cannot set password via update_many. Use single update instead.".into(),
                ));
            }

            let join_data = lua_table_to_json_map(lua, &data_table)?;

            // Build hook data (JSON values for hooks to see)
            let mut base_hook_data: HashMap<String, Value> = data
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            for (k, v) in &join_data {
                base_hook_data.insert(k.clone(), v.clone());
            }

            // Field-level write access: strip denied fields (parity with single update)
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let denied = check_field_write_access_with_lua(
                    lua,
                    &def.fields,
                    user_doc.as_ref(),
                    "update",
                );
                for name in &denied {
                    data.remove(name);
                    base_hook_data.remove(name);
                }
            }

            let mut modified = 0i64;

            // Full per-document lifecycle (parity with single update and gRPC update_many):
            // 1. Field-level before_validate hooks
            // 2. Collection-level BeforeValidate hook
            // 3. validate_fields_inner
            // 4. Field-level before_change hooks
            // 5. Collection-level BeforeChange hook (capture modified data)
            // 6. DB write with hook-modified data
            // 7. After-change hooks
            for doc in &docs {
                let mut hook_data = base_hook_data.clone();

                if hooks_enabled {
                    // Field-level before_validate
                    run_field_hooks_inner(
                        lua,
                        &def.fields,
                        &FieldHookEvent::BeforeValidate,
                        &mut hook_data,
                        &collection,
                        "update",
                    )
                    .map_err(|e| {
                        RuntimeError(format!("before_validate field hook error: {}", e))
                    })?;

                    // Collection-level BeforeValidate
                    let hook_ctx = HookContext::builder(&collection, "update")
                        .data(hook_data.clone())
                        .locale(locale_str.as_deref())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeValidate, hook_ctx)
                        .map_err(|e| RuntimeError(format!("before_validate hook error: {}", e)))?;
                    hook_data = ctx.data;
                }

                // Validation (always runs unless hooks=false)
                if run_hooks {
                    let r = reg
                        .read()
                        .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;
                    let val_ctx = ValidationCtx::builder(conn, &collection)
                        .exclude_id(Some(&doc.id))
                        .locale_ctx(locale_ctx.as_ref())
                        .soft_delete(def.soft_delete)
                        .draft(draft)
                        .registry(&r)
                        .build();
                    validate_fields_inner(lua, &def.fields, &hook_data, &val_ctx)
                        .map_err(|e| RuntimeError(format!("validation error: {}", e)))?;
                }

                if hooks_enabled {
                    // Field-level before_change
                    run_field_hooks_inner(
                        lua,
                        &def.fields,
                        &FieldHookEvent::BeforeChange,
                        &mut hook_data,
                        &collection,
                        "update",
                    )
                    .map_err(|e| RuntimeError(format!("before_change field hook error: {}", e)))?;

                    // Collection-level BeforeChange (capture modified data)
                    let hook_ctx = HookContext::builder(&collection, "update")
                        .data(hook_data.clone())
                        .locale(locale_str.as_deref())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeChange, hook_ctx)
                        .map_err(|e| {
                        RuntimeError(format!("before_change hook error: {}", e))
                    })?;
                    hook_data = ctx.data;
                }

                // Convert hook-processed data back to string map for DB write
                let final_data = HookContext::builder(&collection, "update")
                    .data(hook_data.clone())
                    .build()
                    .to_string_map(&def.fields);

                let old_refs = query::ref_count::snapshot_outgoing_refs(
                    conn,
                    &collection,
                    &doc.id,
                    &def.fields,
                    &lc,
                )
                .map_err(|e| RuntimeError(format!("ref count snapshot error: {}", e)))?;

                let updated = query::update_partial(
                    conn,
                    &collection,
                    &def,
                    &doc.id,
                    &final_data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| RuntimeError(format!("update error: {}", e)))?;
                query::save_join_table_data(
                    conn,
                    &collection,
                    &def.fields,
                    &doc.id,
                    &hook_data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| RuntimeError(format!("join data error: {}", e)))?;

                query::ref_count::after_update(
                    conn,
                    &collection,
                    &doc.id,
                    &def.fields,
                    &lc,
                    old_refs,
                )
                .map_err(|e| RuntimeError(format!("ref count update error: {}", e)))?;

                if conn.supports_fts() {
                    query::fts::fts_upsert(conn, &collection, &updated, Some(&def))
                        .map_err(|e| RuntimeError(format!("FTS upsert error: {}", e)))?;
                }

                if def.has_versions() {
                    let vs_ctx = versions::VersionSnapshotCtx::builder(&collection, &updated.id)
                        .fields(&def.fields)
                        .versions(def.versions.as_ref())
                        .has_drafts(def.has_drafts())
                        .build();
                    versions::create_version_snapshot(conn, &vs_ctx, "published", &updated)
                        .map_err(|e| RuntimeError(format!("version snapshot error: {}", e)))?;
                }

                if hooks_enabled {
                    let mut after_data = updated.fields.clone();
                    run_field_hooks_inner(
                        lua,
                        &def.fields,
                        &FieldHookEvent::AfterChange,
                        &mut after_data,
                        &collection,
                        "update",
                    )
                    .map_err(|e| RuntimeError(format!("after_change field hook error: {}", e)))?;

                    after_data.insert("id".to_string(), Value::String(updated.id.to_string()));
                    let after_ctx = HookContext::builder(&collection, "update")
                        .data(after_data)
                        .locale(locale_str.as_deref())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                        .map_err(|e| RuntimeError(format!("after_change hook error: {}", e)))?;
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
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let delete_many_fn = lua.create_function(
        move |lua, (collection, query_table, opts): (String, Table, Option<Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(false);

            let run_hooks: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
                .unwrap_or(true);

            let force_hard_delete: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("forceHardDelete").ok().flatten())
                .unwrap_or(false);

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
                    .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| RuntimeError(format!("Collection '{}' not found", collection)))?
            };

            // Determine effective soft_delete behavior
            let soft_delete = def.soft_delete && !force_hard_delete;

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

            // Soft delete uses trash permission (fallback to update);
            // hard delete uses delete permission
            let access_ref = if soft_delete {
                def.access.resolve_trash()
            } else {
                def.access.delete.as_deref()
            };

            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, access_ref, user_doc.as_ref(), None, None)
                    .map_err(|e| RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => {
                        return Err(RuntimeError("Delete access denied".into()));
                    }
                    AccessResult::Constrained(extra) => find_query.filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            let mut find_all = FindQuery::new();
            find_all.filters = find_query.filters;
            let docs = query::find(conn, &collection, &def, &find_all, locale_ctx.as_ref())
                .map_err(|e| RuntimeError(format!("find error: {}", e)))?;

            // Check per-doc delete access (all-or-nothing)
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                for doc in &docs {
                    let result = check_access_with_lua(
                        lua,
                        access_ref,
                        user_doc.as_ref(),
                        Some(&doc.id),
                        None,
                    )
                    .map_err(|e| RuntimeError(format!("access check error: {}", e)))?;

                    if matches!(result, AccessResult::Denied) {
                        return Err(RuntimeError(format!(
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

            let is_upload = def.is_upload_collection();
            let config_dir_path = if is_upload {
                lua.app_data_ref::<ConfigDir>().map(|cd| cd.0.clone())
            } else {
                None
            };

            let mut deleted = 0i64;
            for doc in &docs {
                // Ref count protection only applies to hard deletes — soft-deleted
                // docs remain referenceable (ref counts are NOT decremented).
                if !soft_delete {
                    let ref_count = query::ref_count::get_ref_count(conn, &collection, &doc.id)
                        .map_err(|e| RuntimeError(format!("ref count check error: {}", e)))?;
                    if ref_count > 0 {
                        tracing::debug!(
                            "Skipping delete of {}/{}: referenced by {} document(s)",
                            collection,
                            doc.id,
                            ref_count
                        );
                        continue;
                    }
                }

                if hooks_enabled {
                    let hook_ctx = HookContext::builder(&collection, "delete")
                        .data([("id".to_string(), Value::String(doc.id.to_string()))].into())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::BeforeDelete, hook_ctx)
                        .map_err(|e| RuntimeError(format!("before_delete hook error: {}", e)))?;
                }

                // Decrement ref counts before hard delete (CASCADE removes junction rows).
                // Soft delete does NOT adjust ref counts.
                if !soft_delete {
                    query::ref_count::before_hard_delete(
                        conn,
                        &collection,
                        &doc.id,
                        &def.fields,
                        &lc,
                    )
                    .map_err(|e| RuntimeError(format!("ref count error: {}", e)))?;
                }

                if soft_delete {
                    query::soft_delete(conn, &collection, &doc.id)
                        .map_err(|e| RuntimeError(format!("soft_delete error: {}", e)))?;
                } else {
                    query::delete(conn, &collection, &doc.id)
                        .map_err(|e| RuntimeError(format!("delete error: {}", e)))?;
                }

                if conn.supports_fts() {
                    query::fts::fts_delete(conn, &collection, &doc.id)
                        .map_err(|e| RuntimeError(format!("FTS delete error: {}", e)))?;
                }

                // Clean up upload files after successful DB delete (skip for soft-delete)
                if !soft_delete && let Some(dir) = &config_dir_path {
                    upload::delete_upload_files(dir, &doc.fields);
                }

                if hooks_enabled {
                    let after_ctx = HookContext::builder(&collection, "delete")
                        .data([("id".to_string(), Value::String(doc.id.to_string()))].into())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::AfterDelete, after_ctx)
                        .map_err(|e| RuntimeError(format!("after_delete hook error: {}", e)))?;
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
