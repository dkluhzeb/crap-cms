//! Registration of `crap.collections.create` and `crap.collections.update` Lua functions.

use anyhow::Result;
use mlua::Lua;
use std::collections::HashMap;

use crate::config::LocaleConfig;
use crate::core::SharedRegistry;
use crate::db::query::{self, AccessResult, LocaleContext};

use super::get_tx_conn;
use crate::hooks::lifecycle::{UserContext, UiLocaleContext, HookDepth, MaxHookDepth};
use crate::hooks::lifecycle::{HookContext, HookEvent, FieldHookEvent};
use crate::hooks::lifecycle::execution::{run_hooks_inner, run_field_hooks_inner};
use crate::hooks::lifecycle::validation::validate_fields_inner;
use crate::hooks::lifecycle::access::{check_access_with_lua, check_field_write_access_with_lua};
use crate::hooks::lifecycle::converters::*;

/// Register `crap.collections.create(collection, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_create(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let create_fn = lua.create_function(move |lua, (collection, data_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        let hook_user = lua.app_data_ref::<UserContext>().and_then(|uc| uc.0.clone());
        let hook_ui_locale = lua.app_data_ref::<UiLocaleContext>().and_then(|uc| uc.0.clone());

        let locale_str: Option<String> = opts.as_ref()
            .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
        let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

        let override_access: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
            .unwrap_or(true);

        let run_hooks: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
            .unwrap_or(true);

        let def = {
            let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                format!("Registry lock: {}", e)
            ))?;
            r.get_collection(&collection)
                .cloned()
                .ok_or_else(|| mlua::Error::RuntimeError(
                    format!("Collection '{}' not found", collection)
                ))?
        };

        let mut data = lua_table_to_hashmap(&data_table)?;
        flatten_lua_groups(&data_table, &def.fields, &mut data)?;

        // Extract password for auth collections (before hooks/data flow)
        let password = if def.is_auth_collection() {
            data.remove("password")
        } else {
            None
        };

        // Enforce collection-level access control when overrideAccess = false
        if !override_access {
            let user_doc = lua.app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let result = check_access_with_lua(lua, def.access.create.as_deref(), user_doc.as_ref(), None, None)
                .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
            if matches!(result, AccessResult::Denied) {
                return Err(mlua::Error::RuntimeError("Create access denied".into()));
            }
        }

        let draft: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("draft").ok().flatten())
            .unwrap_or(false);
        let is_draft = draft && def.has_drafts();

        // Check hook depth for recursion protection
        let current_depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
        let max_depth = lua.app_data_ref::<MaxHookDepth>().map(|d| d.0).unwrap_or(3);
        let hooks_enabled = run_hooks && current_depth < max_depth;

        if run_hooks && current_depth >= max_depth {
            tracing::warn!(
                "Hook depth {} reached max {}, skipping hooks for create on {}",
                current_depth, max_depth, collection
            );
        }

        // Build hook data (JSON values for hooks to see)
        let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let join_data = lua_table_to_json_map(lua, &data_table)?;
        for (k, v) in &join_data {
            hook_data.insert(k.clone(), v.clone());
        }
        // Ensure password doesn't leak into hooks via join_data
        if def.is_auth_collection() {
            hook_data.remove("password");
        }

        // Strip field-level write-denied fields AFTER hook_data is built
        if !override_access {
            let user_doc = lua.app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let denied = check_field_write_access_with_lua(lua, &def.fields, user_doc.as_ref(), "create");
            for name in &denied {
                data.remove(name);
                hook_data.remove(name);
            }
        }

        if hooks_enabled {
            lua.set_app_data(HookDepth(current_depth + 1));

            // Field-level before_validate
            run_field_hooks_inner(
                lua, &def.fields, &FieldHookEvent::BeforeValidate,
                &mut hook_data, &collection, "create",
            ).map_err(|e| mlua::Error::RuntimeError(format!("before_validate field hook error: {}", e)))?;

            // Collection-level before_validate
            let mut builder = HookContext::builder(collection.clone(), "create")
                .data(hook_data.clone())
                .draft(is_draft)
                .user(hook_user.as_ref())
                .ui_locale(hook_ui_locale.as_deref());
            if let Some(ref l) = locale_str {
                builder = builder.locale(l.clone());
            }
            let hook_ctx = builder.build();
            let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeValidate, hook_ctx)
                .map_err(|e| mlua::Error::RuntimeError(format!("before_validate hook error: {}", e)))?;
            hook_data = ctx.data;
        }

        // Validation (always runs unless hooks=false)
        if run_hooks {
            validate_fields_inner(lua, &def.fields, &hook_data, conn, &collection, None, is_draft)
                .map_err(|e| mlua::Error::RuntimeError(format!("validation error: {}", e)))?;
        }

        if hooks_enabled {
            // Field-level before_change
            run_field_hooks_inner(
                lua, &def.fields, &FieldHookEvent::BeforeChange,
                &mut hook_data, &collection, "create",
            ).map_err(|e| mlua::Error::RuntimeError(format!("before_change field hook error: {}", e)))?;

            // Collection-level before_change
            let mut builder = HookContext::builder(collection.clone(), "create")
                .data(hook_data.clone())
                .draft(is_draft)
                .user(hook_user.as_ref())
                .ui_locale(hook_ui_locale.as_deref());
            if let Some(ref l) = locale_str {
                builder = builder.locale(l.clone());
            }
            let hook_ctx = builder.build();
            let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeChange, hook_ctx)
                .map_err(|e| mlua::Error::RuntimeError(format!("before_change hook error: {}", e)))?;
            hook_data = ctx.data;
        }

        // Convert hook-processed data back to string map for query
        let final_data = HookContext::builder(collection.clone(), "create")
            .data(hook_data.clone())
            .build()
            .to_string_map(&def.fields);

        let doc = crate::service::persist_create(
            conn, &collection, &def, &final_data, &hook_data,
            password.as_deref(), locale_ctx.as_ref(), is_draft,
        ).map_err(|e| mlua::Error::RuntimeError(format!("create error: {}", e)))?;

        // After-change hooks
        if hooks_enabled {
            let mut after_data = doc.fields.clone();
            run_field_hooks_inner(
                lua, &def.fields, &FieldHookEvent::AfterChange,
                &mut after_data, &collection, "create",
            ).map_err(|e| mlua::Error::RuntimeError(format!("after_change field hook error: {}", e)))?;

            let mut builder = HookContext::builder(collection.clone(), "create")
                .data(doc.fields.clone())
                .draft(is_draft)
                .user(hook_user.as_ref())
                .ui_locale(hook_ui_locale.as_deref());
            if let Some(ref l) = locale_str {
                builder = builder.locale(l.clone());
            }
            let after_ctx = builder.build();
            run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                .map_err(|e| mlua::Error::RuntimeError(format!("after_change hook error: {}", e)))?;

            lua.set_app_data(HookDepth(current_depth));
        }

        // Hydrate join-table fields before returning
        let mut doc = doc;
        query::hydrate_document(conn, &collection, &def.fields, &mut doc, None, locale_ctx.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;

        document_to_lua_table(lua, &doc)
    })?;
    table.set("create", create_fn)?;
    Ok(())
}

/// Register `crap.collections.update(collection, id, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_update(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let update_fn = lua.create_function(move |lua, (collection, id, data_table, opts): (String, String, mlua::Table, Option<mlua::Table>)| {
        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        let hook_user = lua.app_data_ref::<UserContext>().and_then(|uc| uc.0.clone());
        let hook_ui_locale = lua.app_data_ref::<UiLocaleContext>().and_then(|uc| uc.0.clone());

        let locale_str: Option<String> = opts.as_ref()
            .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
        let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

        let override_access: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
            .unwrap_or(true);

        let run_hooks: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
            .unwrap_or(true);

        let def = {
            let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                format!("Registry lock: {}", e)
            ))?;
            r.get_collection(&collection)
                .cloned()
                .ok_or_else(|| mlua::Error::RuntimeError(
                    format!("Collection '{}' not found", collection)
                ))?
        };

        let mut data = lua_table_to_hashmap(&data_table)?;
        flatten_lua_groups(&data_table, &def.fields, &mut data)?;

        // Extract password for auth collections (before hooks/data flow)
        let password = if def.is_auth_collection() {
            data.remove("password")
        } else {
            None
        };

        // Read unpublish option
        let unpublish: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("unpublish").ok().flatten())
            .unwrap_or(false);

        // Enforce collection-level access control when overrideAccess = false
        if !override_access {
            let user_doc = lua.app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let result = check_access_with_lua(lua, def.access.update.as_deref(), user_doc.as_ref(), Some(&id), None)
                .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
            if matches!(result, AccessResult::Denied) {
                return Err(mlua::Error::RuntimeError("Update access denied".into()));
            }
        }

        let draft: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("draft").ok().flatten())
            .unwrap_or(false);
        let is_draft = draft && def.has_drafts();

        // Check hook depth for recursion protection
        let current_depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
        let max_depth = lua.app_data_ref::<MaxHookDepth>().map(|d| d.0).unwrap_or(3);
        let hooks_enabled = run_hooks && current_depth < max_depth;

        if run_hooks && current_depth >= max_depth {
            tracing::warn!(
                "Hook depth {} reached max {}, skipping hooks for update on {}",
                current_depth, max_depth, collection
            );
        }

        // Handle unpublish: set status to draft, create version, return
        if unpublish && def.has_versions() {
            let existing_doc = query::find_by_id_raw(conn, &collection, &def, &id, None)
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?
                .ok_or_else(|| mlua::Error::RuntimeError(
                    format!("Document {} not found in {}", id, collection)
                ))?;

            let current_depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
            let max_depth = lua.app_data_ref::<MaxHookDepth>().map(|d| d.0).unwrap_or(3);
            let hooks_enabled = run_hooks && current_depth < max_depth;

            if hooks_enabled {
                lua.set_app_data(HookDepth(current_depth + 1));
                let mut builder = HookContext::builder(collection.clone(), "update")
                    .data(existing_doc.fields.clone())
                    .draft(false)
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref());
                if let Some(ref l) = locale_str {
                    builder = builder.locale(l.clone());
                }
                let before_ctx = builder.build();
                run_hooks_inner(lua, &def.hooks, HookEvent::BeforeChange, before_ctx)
                    .map_err(|e| mlua::Error::RuntimeError(format!("before_change hook error: {}", e)))?;
            }

            crate::service::persist_unpublish(conn, &collection, &id, &def)
                .map_err(|e| mlua::Error::RuntimeError(format!("unpublish error: {}", e)))?;

            if hooks_enabled {
                let mut builder = HookContext::builder(collection.clone(), "update")
                    .data(existing_doc.fields.clone())
                    .draft(false)
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref());
                if let Some(ref l) = locale_str {
                    builder = builder.locale(l.clone());
                }
                let after_ctx = builder.build();
                run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                    .map_err(|e| mlua::Error::RuntimeError(format!("after_change hook error: {}", e)))?;
                lua.set_app_data(HookDepth(current_depth));
            }

            return document_to_lua_table(lua, &existing_doc);
        }

        // Build hook data (JSON values for hooks to see)
        let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let join_data = lua_table_to_json_map(lua, &data_table)?;
        for (k, v) in &join_data {
            hook_data.insert(k.clone(), v.clone());
        }
        if def.is_auth_collection() {
            hook_data.remove("password");
        }

        // Strip field-level write-denied fields AFTER hook_data is built
        if !override_access {
            let user_doc = lua.app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let denied = check_field_write_access_with_lua(lua, &def.fields, user_doc.as_ref(), "update");
            for name in &denied {
                data.remove(name);
                hook_data.remove(name);
            }
        }

        if hooks_enabled {
            lua.set_app_data(HookDepth(current_depth + 1));

            run_field_hooks_inner(
                lua, &def.fields, &FieldHookEvent::BeforeValidate,
                &mut hook_data, &collection, "update",
            ).map_err(|e| mlua::Error::RuntimeError(format!("before_validate field hook error: {}", e)))?;

            let mut builder = HookContext::builder(collection.clone(), "update")
                .data(hook_data.clone())
                .draft(is_draft)
                .user(hook_user.as_ref())
                .ui_locale(hook_ui_locale.as_deref());
            if let Some(ref l) = locale_str {
                builder = builder.locale(l.clone());
            }
            let hook_ctx = builder.build();
            let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeValidate, hook_ctx)
                .map_err(|e| mlua::Error::RuntimeError(format!("before_validate hook error: {}", e)))?;
            hook_data = ctx.data;
        }

        if run_hooks {
            validate_fields_inner(lua, &def.fields, &hook_data, conn, &collection, Some(&id), is_draft)
                .map_err(|e| mlua::Error::RuntimeError(format!("validation error: {}", e)))?;
        }

        if hooks_enabled {
            run_field_hooks_inner(
                lua, &def.fields, &FieldHookEvent::BeforeChange,
                &mut hook_data, &collection, "update",
            ).map_err(|e| mlua::Error::RuntimeError(format!("before_change field hook error: {}", e)))?;

            let mut builder = HookContext::builder(collection.clone(), "update")
                .data(hook_data.clone())
                .draft(is_draft)
                .user(hook_user.as_ref())
                .ui_locale(hook_ui_locale.as_deref());
            if let Some(ref l) = locale_str {
                builder = builder.locale(l.clone());
            }
            let hook_ctx = builder.build();
            let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeChange, hook_ctx)
                .map_err(|e| mlua::Error::RuntimeError(format!("before_change hook error: {}", e)))?;
            hook_data = ctx.data;
        }

        let final_data = HookContext::builder(collection.clone(), "update")
            .data(hook_data.clone())
            .build()
            .to_string_map(&def.fields);

        if is_draft && def.has_versions() {
            let existing_doc = crate::service::persist_draft_version(
                conn, &collection, &id, &def, &hook_data, locale_ctx.as_ref(),
            ).map_err(|e| mlua::Error::RuntimeError(format!("draft version error: {}", e)))?;

            if hooks_enabled {
                let mut builder = HookContext::builder(collection.clone(), "update")
                    .data(existing_doc.fields.clone())
                    .draft(is_draft)
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref());
                if let Some(ref l) = locale_str {
                    builder = builder.locale(l.clone());
                }
                let after_ctx = builder.build();
                run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                    .map_err(|e| mlua::Error::RuntimeError(format!("after_change hook error: {}", e)))?;
                lua.set_app_data(HookDepth(current_depth));
            }

            let mut existing_doc = existing_doc;
            query::hydrate_document(conn, &collection, &def.fields, &mut existing_doc, None, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;

            document_to_lua_table(lua, &existing_doc)
        } else {
            let doc = crate::service::persist_update(
                conn, &collection, &id, &def, &final_data, &hook_data,
                password.as_deref(), locale_ctx.as_ref(),
            ).map_err(|e| mlua::Error::RuntimeError(format!("update error: {}", e)))?;

            if hooks_enabled {
                let mut after_data = doc.fields.clone();
                run_field_hooks_inner(
                    lua, &def.fields, &FieldHookEvent::AfterChange,
                    &mut after_data, &collection, "update",
                ).map_err(|e| mlua::Error::RuntimeError(format!("after_change field hook error: {}", e)))?;

                let mut builder = HookContext::builder(collection.clone(), "update")
                    .data(doc.fields.clone())
                    .draft(is_draft)
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref());
                if let Some(ref l) = locale_str {
                    builder = builder.locale(l.clone());
                }
                let after_ctx = builder.build();
                run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                    .map_err(|e| mlua::Error::RuntimeError(format!("after_change hook error: {}", e)))?;
                lua.set_app_data(HookDepth(current_depth));
            }

            let mut doc = doc;
            query::hydrate_document(conn, &collection, &def.fields, &mut doc, None, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        }
    })?;
    table.set("update", update_fn)?;
    Ok(())
}
