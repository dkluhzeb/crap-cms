//! Registration of `crap.collections.create` and `crap.collections.update` Lua functions.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, SharedRegistry},
    db::{AccessResult, DbConnection, LocaleContext, query},
    hooks::{
        HookContext, HookEvent, ValidationCtx,
        lifecycle::{
            FieldHookEvent, HookDepth, HookDepthGuard, MaxHookDepth, UiLocaleContext, UserContext,
            access::{
                check_access_with_lua, check_field_read_access_with_lua,
                check_field_write_access_with_lua,
            },
            converters::*,
            execution::{run_field_hooks_inner, run_hooks_inner},
            validation::validate_fields_inner,
        },
    },
    service::{self, PersistOptions},
};

use super::get_tx_conn;

use super::unpublish_ctx_builder::UnpublishCtxBuilder;

/// Parameters for the unpublish operation.
pub(super) struct UnpublishCtx<'a> {
    pub(super) collection: &'a str,
    pub(super) id: &'a str,
    pub(super) def: &'a CollectionDefinition,
    pub(super) run_hooks: bool,
    pub(super) locale_str: Option<&'a str>,
    pub(super) hook_user: Option<&'a Document>,
    pub(super) hook_ui_locale: Option<&'a str>,
}

impl<'a> UnpublishCtx<'a> {
    fn builder(
        collection: &'a str,
        id: &'a str,
        def: &'a CollectionDefinition,
    ) -> UnpublishCtxBuilder<'a> {
        UnpublishCtxBuilder::new(collection, id, def)
    }
}

/// Handle the unpublish code path: revert to draft, fire hooks, return document.
fn handle_unpublish(
    lua: &Lua,
    conn: &dyn DbConnection,
    ctx: &UnpublishCtx,
) -> mlua::Result<mlua::Table> {
    let existing_doc = query::find_by_id_raw(conn, ctx.collection, ctx.def, ctx.id, None, false)
        .map_err(|e| RuntimeError(format!("find error: {:#}", e)))?
        .ok_or_else(|| {
            RuntimeError(format!(
                "Document {} not found in {}",
                ctx.id, ctx.collection
            ))
        })?;

    let current_depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
    let max_depth = lua.app_data_ref::<MaxHookDepth>().map(|d| d.0).unwrap_or(3);
    let hooks_enabled = ctx.run_hooks && current_depth < max_depth;

    let _depth_guard = if hooks_enabled {
        Some(HookDepthGuard::increment(lua, current_depth))
    } else {
        None
    };

    if hooks_enabled {
        let before_ctx = HookContext::builder(ctx.collection, "update")
            .data(existing_doc.fields.clone())
            .draft(true)
            .locale(ctx.locale_str)
            .user(ctx.hook_user)
            .ui_locale(ctx.hook_ui_locale)
            .build();
        run_hooks_inner(lua, &ctx.def.hooks, HookEvent::BeforeChange, before_ctx)
            .map_err(|e| RuntimeError(format!("before_change hook error: {:#}", e)))?;
    }

    service::persist_unpublish(conn, ctx.collection, ctx.id, ctx.def)
        .map_err(|e| RuntimeError(format!("unpublish error: {:#}", e)))?;

    // Re-read the document after unpublish so hooks see the updated state
    let updated_doc = query::find_by_id_raw(conn, ctx.collection, ctx.def, ctx.id, None, false)
        .map_err(|e| RuntimeError(format!("find error after unpublish: {:#}", e)))?
        .ok_or_else(|| {
            RuntimeError(format!(
                "Document {} not found after unpublish in {}",
                ctx.id, ctx.collection
            ))
        })?;

    if hooks_enabled {
        let mut after_data = updated_doc.fields.clone();
        after_data.insert("id".to_string(), Value::String(ctx.id.to_string()));

        let after_ctx = HookContext::builder(ctx.collection, "update")
            .data(after_data)
            .draft(true)
            .locale(ctx.locale_str)
            .user(ctx.hook_user)
            .ui_locale(ctx.hook_ui_locale)
            .build();
        run_hooks_inner(lua, &ctx.def.hooks, HookEvent::AfterChange, after_ctx)
            .map_err(|e| RuntimeError(format!("after_change hook error: {:#}", e)))?;
    }

    document_to_lua_table(lua, &updated_doc)
}

/// Register `crap.collections.create(collection, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_create(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let create_fn = lua.create_function(
        move |lua, (collection, data_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

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

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(false);

            let run_hooks: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
                .unwrap_or(true);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| RuntimeError(format!("Registry lock: {:#}", e)))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| RuntimeError(format!("Collection '{}' not found", collection)))?
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
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(
                    lua,
                    def.access.create.as_deref(),
                    user_doc.as_ref(),
                    None,
                    None,
                )
                .map_err(|e| RuntimeError(format!("access check error: {:#}", e)))?;

                if matches!(result, AccessResult::Denied) {
                    return Err(RuntimeError("Create access denied".into()));
                }
            }

            let draft: bool = opts
                .as_ref()
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
                    current_depth,
                    max_depth,
                    collection
                );
            }

            // Build hook data (JSON values for hooks to see)
            let mut hook_data: HashMap<String, Value> = data
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
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
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let denied = check_field_write_access_with_lua(
                    lua,
                    &def.fields,
                    user_doc.as_ref(),
                    "create",
                );
                for name in &denied {
                    data.remove(name);
                    hook_data.remove(name);
                }
            }

            let _depth_guard = if hooks_enabled {
                Some(HookDepthGuard::increment(lua, current_depth))
            } else {
                None
            };

            if hooks_enabled {
                // Field-level before_validate
                run_field_hooks_inner(
                    lua,
                    &def.fields,
                    &FieldHookEvent::BeforeValidate,
                    &mut hook_data,
                    &collection,
                    "create",
                )
                .map_err(|e| RuntimeError(format!("before_validate field hook error: {:#}", e)))?;

                // Collection-level before_validate
                let hook_ctx = HookContext::builder(collection.clone(), "create")
                    .data(hook_data.clone())
                    .draft(is_draft)
                    .locale(locale_str.clone())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeValidate, hook_ctx)
                    .map_err(|e| RuntimeError(format!("before_validate hook error: {:#}", e)))?;
                hook_data = ctx.data;
            }

            // Validation (always runs unless hooks=false)
            if run_hooks {
                let r = reg
                    .read()
                    .map_err(|e| RuntimeError(format!("Registry lock: {:#}", e)))?;
                let val_ctx = ValidationCtx::builder(conn, &collection)
                    .draft(is_draft)
                    .locale_ctx(locale_ctx.as_ref())
                    .registry(&r)
                    .soft_delete(def.soft_delete)
                    .build();
                validate_fields_inner(lua, &def.fields, &hook_data, &val_ctx)
                    .map_err(|e| RuntimeError(format!("validation error: {:#}", e)))?;
            }

            if hooks_enabled {
                // Field-level before_change
                run_field_hooks_inner(
                    lua,
                    &def.fields,
                    &FieldHookEvent::BeforeChange,
                    &mut hook_data,
                    &collection,
                    "create",
                )
                .map_err(|e| RuntimeError(format!("before_change field hook error: {:#}", e)))?;

                // Collection-level before_change
                let hook_ctx = HookContext::builder(collection.clone(), "create")
                    .data(hook_data.clone())
                    .draft(is_draft)
                    .locale(locale_str.clone())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeChange, hook_ctx)
                    .map_err(|e| RuntimeError(format!("before_change hook error: {:#}", e)))?;
                hook_data = ctx.data;
            }

            // Convert hook-processed data back to string map for query
            let final_data = HookContext::builder(collection.clone(), "create")
                .data(hook_data.clone())
                .build()
                .to_string_map(&def.fields);

            let persist_opts = PersistOptions::builder()
                .password(password.as_deref())
                .locale_ctx(locale_ctx.as_ref())
                .locale_config(&lc)
                .draft(is_draft)
                .build();
            let doc = service::persist_create(
                conn,
                &collection,
                &def,
                &final_data,
                &hook_data,
                &persist_opts,
            )
            .map_err(|e| RuntimeError(format!("create error: {:#}", e)))?;

            // After-change hooks
            if hooks_enabled {
                let mut after_data = doc.fields.clone();
                after_data.insert("id".to_string(), Value::String(doc.id.to_string()));

                run_field_hooks_inner(
                    lua,
                    &def.fields,
                    &FieldHookEvent::AfterChange,
                    &mut after_data,
                    &collection,
                    "create",
                )
                .map_err(|e| RuntimeError(format!("after_change field hook error: {:#}", e)))?;

                let after_ctx = HookContext::builder(collection.clone(), "create")
                    .data(after_data)
                    .draft(is_draft)
                    .locale(locale_str.clone())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                    .map_err(|e| RuntimeError(format!("after_change hook error: {:#}", e)))?;
            }

            // Hydrate join-table fields before returning
            let mut doc = doc;
            query::hydrate_document(
                conn,
                &collection,
                &def.fields,
                &mut doc,
                None,
                locale_ctx.as_ref(),
            )
            .map_err(|e| RuntimeError(format!("hydrate error: {:#}", e)))?;

            // Strip field-level read-denied fields from returned document
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let denied = check_field_read_access_with_lua(lua, &def.fields, user_doc.as_ref());
                for name in &denied {
                    doc.fields.remove(name);
                }
            }

            document_to_lua_table(lua, &doc)
        },
    )?;
    table.set("create", create_fn)?;
    Ok(())
}

/// Register `crap.collections.update(collection, id, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_update(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
        move |lua, (collection, id, data_table, opts): (String, String, Table, Option<Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

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

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(false);

            let run_hooks: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("hooks").ok().flatten())
                .unwrap_or(true);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| RuntimeError(format!("Registry lock: {:#}", e)))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| RuntimeError(format!("Collection '{}' not found", collection)))?
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
            let unpublish: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("unpublish").ok().flatten())
                .unwrap_or(false);

            // Enforce collection-level access control when overrideAccess = false
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(
                    lua,
                    def.access.update.as_deref(),
                    user_doc.as_ref(),
                    Some(&id),
                    None,
                )
                .map_err(|e| RuntimeError(format!("access check error: {:#}", e)))?;

                if matches!(result, AccessResult::Denied) {
                    return Err(RuntimeError("Update access denied".into()));
                }
            }

            let draft: bool = opts
                .as_ref()
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
                    current_depth,
                    max_depth,
                    collection
                );
            }

            // Handle unpublish: set status to draft, create version, return
            if unpublish && def.has_versions() {
                return handle_unpublish(
                    lua,
                    conn,
                    &UnpublishCtx::builder(&collection, &id, &def)
                        .run_hooks(run_hooks)
                        .locale_str(locale_str.as_deref())
                        .hook_user(hook_user.as_ref())
                        .hook_ui_locale(hook_ui_locale.as_deref())
                        .build(),
                );
            }

            // Build hook data (JSON values for hooks to see)
            let mut hook_data: HashMap<String, Value> = data
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
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
                    hook_data.remove(name);
                }
            }

            let _depth_guard = if hooks_enabled {
                Some(HookDepthGuard::increment(lua, current_depth))
            } else {
                None
            };

            if hooks_enabled {
                run_field_hooks_inner(
                    lua,
                    &def.fields,
                    &FieldHookEvent::BeforeValidate,
                    &mut hook_data,
                    &collection,
                    "update",
                )
                .map_err(|e| RuntimeError(format!("before_validate field hook error: {:#}", e)))?;

                let hook_ctx = HookContext::builder(collection.clone(), "update")
                    .data(hook_data.clone())
                    .draft(is_draft)
                    .locale(locale_str.clone())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeValidate, hook_ctx)
                    .map_err(|e| RuntimeError(format!("before_validate hook error: {:#}", e)))?;
                hook_data = ctx.data;
            }

            if run_hooks {
                let r = reg
                    .read()
                    .map_err(|e| RuntimeError(format!("Registry lock: {:#}", e)))?;
                let val_ctx = ValidationCtx::builder(conn, &collection)
                    .exclude_id(Some(&id))
                    .draft(is_draft)
                    .locale_ctx(locale_ctx.as_ref())
                    .registry(&r)
                    .soft_delete(def.soft_delete)
                    .build();
                validate_fields_inner(lua, &def.fields, &hook_data, &val_ctx)
                    .map_err(|e| RuntimeError(format!("validation error: {:#}", e)))?;
            }

            if hooks_enabled {
                run_field_hooks_inner(
                    lua,
                    &def.fields,
                    &FieldHookEvent::BeforeChange,
                    &mut hook_data,
                    &collection,
                    "update",
                )
                .map_err(|e| RuntimeError(format!("before_change field hook error: {:#}", e)))?;

                let hook_ctx = HookContext::builder(collection.clone(), "update")
                    .data(hook_data.clone())
                    .draft(is_draft)
                    .locale(locale_str.clone())
                    .user(hook_user.as_ref())
                    .ui_locale(hook_ui_locale.as_deref())
                    .build();
                let ctx = run_hooks_inner(lua, &def.hooks, HookEvent::BeforeChange, hook_ctx)
                    .map_err(|e| RuntimeError(format!("before_change hook error: {:#}", e)))?;
                hook_data = ctx.data;
            }

            let final_data = HookContext::builder(collection.clone(), "update")
                .data(hook_data.clone())
                .build()
                .to_string_map(&def.fields);

            if is_draft && def.has_versions() {
                let existing_doc = service::persist_draft_version(
                    conn,
                    &collection,
                    &id,
                    &def,
                    &hook_data,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| RuntimeError(format!("draft version error: {:#}", e)))?;

                if hooks_enabled {
                    let mut after_data = existing_doc.fields.clone();
                    after_data.insert("id".to_string(), Value::String(id.to_string()));

                    run_field_hooks_inner(
                        lua,
                        &def.fields,
                        &FieldHookEvent::AfterChange,
                        &mut after_data,
                        &collection,
                        "update",
                    )
                    .map_err(|e| RuntimeError(format!("after_change field hook error: {:#}", e)))?;

                    let after_ctx = HookContext::builder(collection.clone(), "update")
                        .data(after_data)
                        .draft(is_draft)
                        .locale(locale_str.clone())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                        .map_err(|e| RuntimeError(format!("after_change hook error: {:#}", e)))?;
                }

                let mut existing_doc = existing_doc;
                query::hydrate_document(
                    conn,
                    &collection,
                    &def.fields,
                    &mut existing_doc,
                    None,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| RuntimeError(format!("hydrate error: {:#}", e)))?;

                if !override_access {
                    let user_doc = lua
                        .app_data_ref::<UserContext>()
                        .and_then(|uc| uc.0.clone());
                    let denied =
                        check_field_read_access_with_lua(lua, &def.fields, user_doc.as_ref());
                    for name in &denied {
                        existing_doc.fields.remove(name);
                    }
                }

                document_to_lua_table(lua, &existing_doc)
            } else {
                let doc = service::persist_update(
                    conn,
                    &collection,
                    &id,
                    &def,
                    &final_data,
                    &hook_data,
                    &service::PersistOptions::builder()
                        .password(password.as_deref())
                        .locale_ctx(locale_ctx.as_ref())
                        .locale_config(&lc)
                        .build(),
                )
                .map_err(|e| RuntimeError(format!("update error: {:#}", e)))?;

                if hooks_enabled {
                    let mut after_data = doc.fields.clone();
                    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));

                    run_field_hooks_inner(
                        lua,
                        &def.fields,
                        &FieldHookEvent::AfterChange,
                        &mut after_data,
                        &collection,
                        "update",
                    )
                    .map_err(|e| RuntimeError(format!("after_change field hook error: {:#}", e)))?;

                    let after_ctx = HookContext::builder(collection.clone(), "update")
                        .data(after_data)
                        .draft(is_draft)
                        .locale(locale_str.clone())
                        .user(hook_user.as_ref())
                        .ui_locale(hook_ui_locale.as_deref())
                        .build();
                    run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, after_ctx)
                        .map_err(|e| RuntimeError(format!("after_change hook error: {:#}", e)))?;
                }

                let mut doc = doc;
                query::hydrate_document(
                    conn,
                    &collection,
                    &def.fields,
                    &mut doc,
                    None,
                    locale_ctx.as_ref(),
                )
                .map_err(|e| RuntimeError(format!("hydrate error: {:#}", e)))?;

                if !override_access {
                    let user_doc = lua
                        .app_data_ref::<UserContext>()
                        .and_then(|uc| uc.0.clone());
                    let denied =
                        check_field_read_access_with_lua(lua, &def.fields, user_doc.as_ref());
                    for name in &denied {
                        doc.fields.remove(name);
                    }
                }

                document_to_lua_table(lua, &doc)
            }
        },
    )?;
    table.set("update", update_fn)?;
    Ok(())
}
