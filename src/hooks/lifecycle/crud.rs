//! Lua CRUD function registration and type conversion helpers.

use anyhow::Result;
use mlua::{Lua, Value};
use std::collections::HashMap;

use crate::config::LocaleConfig;
use crate::core::SharedRegistry;
use crate::db::query::{self, AccessResult, FindQuery, Filter, FilterOp, FilterClause, LocaleContext};

use super::TxContext;
use super::UserContext;
use super::access::{check_access_with_lua, check_field_read_access_with_lua, check_field_write_access_with_lua};

/// Get the active transaction connection from Lua app_data.
/// Returns an error if called outside of `run_hooks_with_conn`.
pub(crate) fn get_tx_conn(lua: &Lua) -> mlua::Result<*const rusqlite::Connection> {
    let ctx = lua.app_data_ref::<TxContext>()
        .ok_or_else(|| mlua::Error::RuntimeError(
            "crap.collections CRUD functions are only available inside hooks \
             with transaction context (before_change, before_delete, etc.)"
                .into()
        ))?;
    Ok(ctx.0)
}

/// Register the CRUD functions on `crap.collections` and `crap.globals`.
/// They read the active connection from Lua app_data (set by `run_hooks_with_conn`).
pub(crate) fn register_crud_functions(lua: &Lua, registry: SharedRegistry, locale_config: &LocaleConfig) -> Result<()> {
    let crap: mlua::Table = lua.globals().get("crap")?;
    let collections: mlua::Table = crap.get("collections")?;

    // crap.collections.find(collection, query?)
    // query.depth (optional, default 0): populate relationship fields to this depth
    // query.locale (optional): locale code or "all"
    // query.overrideAccess (optional, default true): bypass access control
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let find_fn = lua.create_function(move |lua, (collection, query_table): (String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            // Safety: pointer is valid while TxContext is in app_data
            let conn = unsafe { &*conn_ptr };

            let depth: i32 = query_table.as_ref()
                .and_then(|qt| qt.get::<i32>("depth").ok())
                .unwrap_or(0)
                .clamp(0, 10);

            let locale_str: Option<String> = query_table.as_ref()
                .and_then(|qt| qt.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = query_table.as_ref()
                .and_then(|qt| qt.get::<Option<bool>>("overrideAccess").ok().flatten())
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

            let mut find_query = match query_table {
                Some(qt) => lua_table_to_find_query(&qt)?,
                None => FindQuery::default(),
            };

            // Enforce access control when overrideAccess = false
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.read.as_deref(), user_doc.as_ref(), None, None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => return Err(mlua::Error::RuntimeError("Read access denied".into())),
                    AccessResult::Constrained(extra) => find_query.filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            query::validate_query_fields(&def, &find_query, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;

            let mut docs = query::find(conn, &collection, &def, &find_query, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;
            let total = query::count(conn, &collection, &def, &find_query.filters, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("count error: {}", e)))?;

            // Hydrate join table data + populate relationships
            let select_slice = find_query.select.as_deref();
            for doc in &mut docs {
                query::hydrate_document(conn, &collection, &def, doc, select_slice, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;
            }
            if depth > 0 {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                for doc in &mut docs {
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        conn, &r, &collection, &def, doc, depth, &mut visited, select_slice,
                    ).map_err(|e| mlua::Error::RuntimeError(format!("populate error: {}", e)))?;
                }
            }
            // Apply select field stripping for find results
            if let Some(ref sel) = find_query.select {
                for doc in &mut docs {
                    query::apply_select_to_document(doc, sel);
                }
            }

            // Field-level read stripping when overrideAccess = false
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let denied = check_field_read_access_with_lua(lua, &def.fields, user_doc.as_ref());
                if !denied.is_empty() {
                    for doc in &mut docs {
                        for name in &denied {
                            doc.fields.remove(name);
                        }
                    }
                }
            }

            find_result_to_lua(lua, &docs, total)
        })?;
        collections.set("find", find_fn)?;
    }

    // crap.collections.find_by_id(collection, id, opts?)
    // opts.depth (optional, default 0): populate relationship fields to this depth
    // opts.locale (optional): locale code or "all"
    // opts.overrideAccess (optional, default true): bypass access control
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let find_by_id_fn = lua.create_function(move |lua, (collection, id, opts): (String, String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let depth: i32 = opts.as_ref()
                .and_then(|o| o.get::<i32>("depth").ok())
                .unwrap_or(0)
                .clamp(0, 10);

            let locale_str: Option<String> = opts.as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
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

            let select: Option<Vec<String>> = opts.as_ref()
                .and_then(|o| o.get::<mlua::Table>("select").ok())
                .map(|t| t.sequence_values::<String>().filter_map(|r| r.ok()).collect());

            // Check access and determine constraints
            let access_constraints = if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.read.as_deref(), user_doc.as_ref(), Some(&id), None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => return Err(mlua::Error::RuntimeError("Read access denied".into())),
                    AccessResult::Constrained(extra) => Some(extra),
                    AccessResult::Allowed => None,
                }
            } else {
                None
            };

            // If constrained, use find with id filter + constraints
            let mut doc = if let Some(constraints) = access_constraints {
                let mut filters = constraints;
                filters.push(FilterClause::Single(Filter {
                    field: "id".to_string(),
                    op: FilterOp::Equals(id.clone()),
                }));
                let query = FindQuery { filters, ..Default::default() };
                let docs = query::find(conn, &collection, &def, &query, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;
                docs.into_iter().next()
            } else {
                query::find_by_id(conn, &collection, &def, &id, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("find_by_id error: {}", e)))?
            };

            if let Some(ref mut d) = doc {
                let select_slice = select.as_deref();
                query::hydrate_document(conn, &collection, &def, d, select_slice, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;
                if depth > 0 {
                    let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                        format!("Registry lock: {}", e)
                    ))?;
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        conn, &r, &collection, &def, d, depth, &mut visited, select_slice,
                    ).map_err(|e| mlua::Error::RuntimeError(format!("populate error: {}", e)))?;
                }
                if let Some(ref sel) = select {
                    query::apply_select_to_document(d, sel);
                }
            }

            // Field-level read stripping when overrideAccess = false
            if !override_access {
                if let Some(ref mut d) = doc {
                    let user_doc = lua.app_data_ref::<UserContext>()
                        .and_then(|uc| uc.0.clone());
                    let denied = check_field_read_access_with_lua(lua, &def.fields, user_doc.as_ref());
                    for name in &denied {
                        d.fields.remove(name);
                    }
                }
            }

            match doc {
                Some(d) => Ok(Value::Table(document_to_lua_table(lua, &d)?)),
                None => Ok(Value::Nil),
            }
        })?;
        collections.set("find_by_id", find_by_id_fn)?;
    }

    // crap.collections.create(collection, data, opts?)
    // opts.locale (optional): locale code to write to
    // opts.overrideAccess (optional, default true): bypass access control
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let create_fn = lua.create_function(move |lua, (collection, data_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts.as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
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

            // Enforce access control when overrideAccess = false
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.create.as_deref(), user_doc.as_ref(), None, None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                if matches!(result, AccessResult::Denied) {
                    return Err(mlua::Error::RuntimeError("Create access denied".into()));
                }
                let denied = check_field_write_access_with_lua(lua, &def.fields, user_doc.as_ref(), "create");
                for name in &denied {
                    data.remove(name);
                }
            }

            let draft: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("draft").ok().flatten())
                .unwrap_or(false);
            let is_draft = draft && def.has_drafts();
            let status = if is_draft { "draft" } else { "published" };

            // Validate required fields when not a draft (draft-enabled collections
            // don't have NOT NULL constraints, so we must check at the app level)
            if !is_draft && def.has_drafts() {
                for field in &def.fields {
                    if field.required && field.field_type != crate::core::field::FieldType::Checkbox {
                        let val = data.get(&field.name);
                        if val.is_none() || val == Some(&String::new()) {
                            return Err(mlua::Error::RuntimeError(
                                format!("Field '{}' is required", field.name)
                            ));
                        }
                    }
                }
            }

            let doc = query::create(conn, &collection, &def, &data, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("create error: {}", e)))?;

            // Save has-many, array, and blocks join-table data
            let join_data = lua_table_to_json_map(lua, &data_table)?;
            query::save_join_table_data(conn, &collection, &def, &doc.id, &join_data, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("join data error: {}", e)))?;

            // Versioning: set status (only if drafts enabled) and create initial version snapshot
            if def.has_versions() {
                if def.has_drafts() {
                    query::set_document_status(conn, &collection, &doc.id, status)
                        .map_err(|e| mlua::Error::RuntimeError(format!("set_status error: {}", e)))?;
                }
                let snapshot = query::build_snapshot(conn, &collection, &def, &doc)
                    .map_err(|e| mlua::Error::RuntimeError(format!("snapshot error: {}", e)))?;
                query::create_version(conn, &collection, &doc.id, status, &snapshot)
                    .map_err(|e| mlua::Error::RuntimeError(format!("version error: {}", e)))?;
                if let Some(ref vc) = def.versions {
                    if vc.max_versions > 0 {
                        query::prune_versions(conn, &collection, &doc.id, vc.max_versions)
                            .map_err(|e| mlua::Error::RuntimeError(format!("prune error: {}", e)))?;
                    }
                }
            }

            // Hydrate join-table fields before returning
            let mut doc = doc;
            query::hydrate_document(conn, &collection, &def, &mut doc, None, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        })?;
        collections.set("create", create_fn)?;
    }

    // crap.collections.update(collection, id, data, opts?)
    // opts.locale (optional): locale code to write to
    // opts.overrideAccess (optional, default true): bypass access control
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let update_fn = lua.create_function(move |lua, (collection, id, data_table, opts): (String, String, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts.as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
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

            // Enforce access control when overrideAccess = false
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.update.as_deref(), user_doc.as_ref(), Some(&id), None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                if matches!(result, AccessResult::Denied) {
                    return Err(mlua::Error::RuntimeError("Update access denied".into()));
                }
                let denied = check_field_write_access_with_lua(lua, &def.fields, user_doc.as_ref(), "update");
                for name in &denied {
                    data.remove(name);
                }
            }

            let draft: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("draft").ok().flatten())
                .unwrap_or(false);
            let is_draft = draft && def.has_drafts();

            if is_draft && def.has_versions() {
                // Version-only save: do NOT update the main table.
                let existing_doc = query::find_by_id(conn, &collection, &def, &id, None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Document {} not found in {}", id, collection)
                    ))?;

                // Merge incoming data onto existing doc fields
                let mut snapshot_fields = existing_doc.fields.clone();
                for (k, v) in &data {
                    snapshot_fields.insert(k.clone(), serde_json::Value::String(v.clone()));
                }
                let snapshot_doc = crate::core::document::Document {
                    id: id.clone(),
                    fields: snapshot_fields,
                    created_at: existing_doc.created_at.clone(),
                    updated_at: existing_doc.updated_at.clone(),
                };

                let snapshot = query::build_snapshot(conn, &collection, &def, &snapshot_doc)
                    .map_err(|e| mlua::Error::RuntimeError(format!("snapshot error: {}", e)))?;
                query::create_version(conn, &collection, &id, "draft", &snapshot)
                    .map_err(|e| mlua::Error::RuntimeError(format!("version error: {}", e)))?;
                if let Some(ref vc) = def.versions {
                    if vc.max_versions > 0 {
                        query::prune_versions(conn, &collection, &id, vc.max_versions)
                            .map_err(|e| mlua::Error::RuntimeError(format!("prune error: {}", e)))?;
                    }
                }

                document_to_lua_table(lua, &existing_doc)
            } else {
                // Normal update: write to main table
                let doc = query::update(conn, &collection, &def, &id, &data, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("update error: {}", e)))?;

                // Save has-many, array, and blocks join-table data
                let join_data = lua_table_to_json_map(lua, &data_table)?;
                query::save_join_table_data(conn, &collection, &def, &doc.id, &join_data, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("join data error: {}", e)))?;

                // Versioning: set status to published (only if drafts enabled) and create version
                if def.has_versions() {
                    if def.has_drafts() {
                        query::set_document_status(conn, &collection, &doc.id, "published")
                            .map_err(|e| mlua::Error::RuntimeError(format!("set_status error: {}", e)))?;
                    }
                    let snapshot = query::build_snapshot(conn, &collection, &def, &doc)
                        .map_err(|e| mlua::Error::RuntimeError(format!("snapshot error: {}", e)))?;
                    query::create_version(conn, &collection, &doc.id, "published", &snapshot)
                        .map_err(|e| mlua::Error::RuntimeError(format!("version error: {}", e)))?;
                    if let Some(ref vc) = def.versions {
                        if vc.max_versions > 0 {
                            query::prune_versions(conn, &collection, &doc.id, vc.max_versions)
                                .map_err(|e| mlua::Error::RuntimeError(format!("prune error: {}", e)))?;
                        }
                    }
                }

                // Hydrate join-table fields before returning
                let mut doc = doc;
                query::hydrate_document(conn, &collection, &def, &mut doc, None, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;

                document_to_lua_table(lua, &doc)
            }
        })?;
        collections.set("update", update_fn)?;
    }

    // crap.collections.delete(collection, id, opts?)
    // opts.overrideAccess (optional, default true): bypass access control
    {
        let reg = registry.clone();
        let delete_fn = lua.create_function(move |lua, (collection, id, opts): (String, String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let override_access: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(true);

            // Enforce access control when overrideAccess = false
            if !override_access {
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
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.delete.as_deref(), user_doc.as_ref(), Some(&id), None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                if matches!(result, AccessResult::Denied) {
                    return Err(mlua::Error::RuntimeError("Delete access denied".into()));
                }
            }

            query::delete(conn, &collection, &id)
                .map_err(|e| mlua::Error::RuntimeError(format!("delete error: {}", e)))?;

            Ok(true)
        })?;
        collections.set("delete", delete_fn)?;
    }

    // crap.collections.count(collection, query?)
    // query.locale (optional): locale code or "all"
    // query.overrideAccess (optional, default true): bypass access control
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let count_fn = lua.create_function(move |lua, (collection, query_table): (String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = query_table.as_ref()
                .and_then(|qt| qt.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = query_table.as_ref()
                .and_then(|qt| qt.get::<Option<bool>>("overrideAccess").ok().flatten())
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

            let mut filters = match query_table {
                Some(ref qt) => lua_table_to_find_query(qt)?.filters,
                None => Vec::new(),
            };

            // Enforce access control when overrideAccess = false
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.read.as_deref(), user_doc.as_ref(), None, None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => return Err(mlua::Error::RuntimeError("Read access denied".into())),
                    AccessResult::Constrained(extra) => filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            let count = query::count(conn, &collection, &def, &filters, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("count error: {}", e)))?;

            Ok(count)
        })?;
        collections.set("count", count_fn)?;
    }

    // crap.collections.update_many(collection, query, data, opts?)
    // Raw bulk update: finds matching docs, checks access, updates each. No per-doc hooks.
    // Returns { modified = N }
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let update_many_fn = lua.create_function(move |lua, (collection, query_table, data_table, opts): (String, mlua::Table, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts.as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
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

            let mut find_query = lua_table_to_find_query(&query_table)?;

            // Find all matching docs first
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.read.as_deref(), user_doc.as_ref(), None, None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => return Err(mlua::Error::RuntimeError("Read access denied".into())),
                    AccessResult::Constrained(extra) => find_query.filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            // Remove limit/offset to get all matching docs
            let find_all = FindQuery { filters: find_query.filters, ..Default::default() };
            let docs = query::find(conn, &collection, &def, &find_all, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;

            // Check per-doc update access (all-or-nothing)
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                for doc in &docs {
                    let result = check_access_with_lua(lua, def.access.update.as_deref(), user_doc.as_ref(), Some(&doc.id), None)
                        .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                    if matches!(result, AccessResult::Denied) {
                        return Err(mlua::Error::RuntimeError(
                            format!("Update access denied for document {}", doc.id)
                        ));
                    }
                }
            }

            let data = lua_table_to_hashmap(&data_table)?;
            let join_data = lua_table_to_json_map(lua, &data_table)?;
            let mut modified = 0i64;

            for doc in &docs {
                query::update(conn, &collection, &def, &doc.id, &data, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("update error: {}", e)))?;
                query::save_join_table_data(conn, &collection, &def, &doc.id, &join_data, locale_ctx.as_ref())
                    .map_err(|e| mlua::Error::RuntimeError(format!("join data error: {}", e)))?;
                modified += 1;
            }

            let result = lua.create_table()?;
            result.set("modified", modified)?;
            Ok(result)
        })?;
        collections.set("update_many", update_many_fn)?;
    }

    // crap.collections.delete_many(collection, query, opts?)
    // Raw bulk delete: finds matching docs, checks access, deletes each. No per-doc hooks.
    // Returns { deleted = N }
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let delete_many_fn = lua.create_function(move |lua, (collection, query_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let override_access: bool = opts.as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(true);

            let locale_str: Option<String> = opts.as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

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

            let mut find_query = lua_table_to_find_query(&query_table)?;

            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(lua, def.access.read.as_deref(), user_doc.as_ref(), None, None)
                    .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                match result {
                    AccessResult::Denied => return Err(mlua::Error::RuntimeError("Read access denied".into())),
                    AccessResult::Constrained(extra) => find_query.filters.extend(extra),
                    AccessResult::Allowed => {}
                }
            }

            let find_all = FindQuery { filters: find_query.filters, ..Default::default() };
            let docs = query::find(conn, &collection, &def, &find_all, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;

            // Check per-doc delete access (all-or-nothing)
            if !override_access {
                let user_doc = lua.app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                for doc in &docs {
                    let result = check_access_with_lua(lua, def.access.delete.as_deref(), user_doc.as_ref(), Some(&doc.id), None)
                        .map_err(|e| mlua::Error::RuntimeError(format!("access check error: {}", e)))?;
                    if matches!(result, AccessResult::Denied) {
                        return Err(mlua::Error::RuntimeError(
                            format!("Delete access denied for document {}", doc.id)
                        ));
                    }
                }
            }

            let mut deleted = 0i64;
            for doc in &docs {
                query::delete(conn, &collection, &doc.id)
                    .map_err(|e| mlua::Error::RuntimeError(format!("delete error: {}", e)))?;
                deleted += 1;
            }

            let result = lua.create_table()?;
            result.set("deleted", deleted)?;
            Ok(result)
        })?;
        collections.set("delete_many", delete_many_fn)?;
    }

    // ── Globals CRUD ─────────────────────────────────────────────────────────

    let globals: mlua::Table = crap.get("globals")?;

    // crap.globals.get(slug, opts?)
    // opts.locale (optional): locale code or "all"
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let get_fn = lua.create_function(move |lua, (slug, opts): (String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts.as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_global(&slug)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Global '{}' not found", slug)
                    ))?
            };

            let doc = query::get_global(conn, &slug, &def, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("get_global error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        })?;
        globals.set("get", get_fn)?;
    }

    // crap.globals.update(slug, data, opts?)
    // opts.locale (optional): locale code to write to
    {
        let reg = registry.clone();
        let lc = locale_config.clone();
        let update_fn = lua.create_function(move |lua, (slug, data_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts.as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_global(&slug)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Global '{}' not found", slug)
                    ))?
            };

            let data = lua_table_to_hashmap(&data_table)?;
            let doc = query::update_global(conn, &slug, &def, &data, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("update_global error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        })?;
        globals.set("update", update_fn)?;
    }

    Ok(())
}

// ── Lua <-> Rust type conversion helpers ────────────────────────────────────

/// Convert a Lua data table to HashMap<String, serde_json::Value>.
/// Preserves nested tables (blocks, arrays, has-many IDs) unlike lua_table_to_hashmap
/// which only handles scalars.
fn lua_table_to_json_map(lua: &Lua, tbl: &mlua::Table) -> mlua::Result<HashMap<String, serde_json::Value>> {
    let mut map = HashMap::new();
    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;
        if matches!(v, Value::Nil) { continue; }
        map.insert(k, crate::hooks::api::lua_to_json(lua, &v)?);
    }
    Ok(map)
}

/// Convert a Lua query table to a FindQuery.
/// Supports both simple filters (`{ status = "published" }`) and operator-based
/// filters (`{ title = { contains = "hello" } }`).
pub(crate) fn lua_table_to_find_query(tbl: &mlua::Table) -> mlua::Result<FindQuery> {
    let filters = if let Ok(filters_tbl) = tbl.get::<mlua::Table>("filters") {
        let mut clauses = Vec::new();
        for pair in filters_tbl.pairs::<String, Value>() {
            let (field, value) = pair?;

            // Handle "or" key for OR groups
            if field == "or" {
                if let Value::Table(or_array) = value {
                    let mut groups = Vec::new();
                    for element in or_array.sequence_values::<mlua::Table>() {
                        let tbl = element?;
                        let mut group = Vec::new();
                        for inner_pair in tbl.pairs::<String, Value>() {
                            let (f, v) = inner_pair?;
                            match v {
                                Value::String(s) => {
                                    group.push(Filter {
                                        field: f,
                                        op: FilterOp::Equals(s.to_str()?.to_string()),
                                    });
                                }
                                Value::Integer(i) => {
                                    group.push(Filter {
                                        field: f,
                                        op: FilterOp::Equals(i.to_string()),
                                    });
                                }
                                Value::Number(n) => {
                                    group.push(Filter {
                                        field: f,
                                        op: FilterOp::Equals(n.to_string()),
                                    });
                                }
                                Value::Table(op_tbl) => {
                                    for op_pair in op_tbl.pairs::<String, Value>() {
                                        let (op_name, op_val) = op_pair?;
                                        let op = lua_parse_filter_op(&op_name, &op_val)?;
                                        group.push(Filter {
                                            field: f.clone(),
                                            op,
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                        groups.push(group);
                    }
                    clauses.push(FilterClause::Or(groups));
                }
                continue;
            }

            match value {
                // Simple string value -> Equals
                Value::String(s) => {
                    clauses.push(FilterClause::Single(Filter {
                        field,
                        op: FilterOp::Equals(s.to_str()?.to_string()),
                    }));
                }
                // Number -> Equals with string representation
                Value::Integer(i) => {
                    clauses.push(FilterClause::Single(Filter {
                        field,
                        op: FilterOp::Equals(i.to_string()),
                    }));
                }
                Value::Number(n) => {
                    clauses.push(FilterClause::Single(Filter {
                        field,
                        op: FilterOp::Equals(n.to_string()),
                    }));
                }
                // Table -> operator-based filter
                Value::Table(op_tbl) => {
                    for op_pair in op_tbl.pairs::<String, Value>() {
                        let (op_name, op_val) = op_pair?;
                        let op = lua_parse_filter_op(&op_name, &op_val)?;
                        clauses.push(FilterClause::Single(Filter {
                            field: field.clone(),
                            op,
                        }));
                    }
                }
                _ => {} // skip nil, bool, etc.
            }
        }
        clauses
    } else {
        Vec::new()
    };

    let order_by: Option<String> = tbl.get("order_by").ok();
    let limit: Option<i64> = tbl.get("limit").ok();
    let offset: Option<i64> = tbl.get("offset").ok();
    let select: Option<Vec<String>> = tbl.get::<mlua::Table>("select").ok()
        .map(|t| t.sequence_values::<String>().filter_map(|r| r.ok()).collect());

    Ok(FindQuery { filters, order_by, limit, offset, select })
}

/// Parse a Lua filter operator name + value into a FilterOp.
pub(crate) fn lua_parse_filter_op(op_name: &str, value: &Value) -> mlua::Result<FilterOp> {
    let to_string = |v: &Value| -> mlua::Result<String> {
        match v {
            Value::String(s) => Ok(s.to_str()?.to_string()),
            Value::Integer(i) => Ok(i.to_string()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Boolean(b) => Ok(b.to_string()),
            _ => Err(mlua::Error::RuntimeError("filter value must be string, number, or boolean".into())),
        }
    };

    match op_name {
        "equals" => Ok(FilterOp::Equals(to_string(value)?)),
        "not_equals" => Ok(FilterOp::NotEquals(to_string(value)?)),
        "like" => Ok(FilterOp::Like(to_string(value)?)),
        "contains" => Ok(FilterOp::Contains(to_string(value)?)),
        "greater_than" => Ok(FilterOp::GreaterThan(to_string(value)?)),
        "less_than" => Ok(FilterOp::LessThan(to_string(value)?)),
        "greater_than_or_equal" => Ok(FilterOp::GreaterThanOrEqual(to_string(value)?)),
        "less_than_or_equal" => Ok(FilterOp::LessThanOrEqual(to_string(value)?)),
        "in" => {
            if let Value::Table(t) = value {
                let mut vals = Vec::new();
                for v in t.clone().sequence_values::<Value>() {
                    vals.push(to_string(&v?)?);
                }
                Ok(FilterOp::In(vals))
            } else {
                Err(mlua::Error::RuntimeError("'in' operator requires a table/array".into()))
            }
        }
        "not_in" => {
            if let Value::Table(t) = value {
                let mut vals = Vec::new();
                for v in t.clone().sequence_values::<Value>() {
                    vals.push(to_string(&v?)?);
                }
                Ok(FilterOp::NotIn(vals))
            } else {
                Err(mlua::Error::RuntimeError("'not_in' operator requires a table/array".into()))
            }
        }
        "exists" => Ok(FilterOp::Exists),
        "not_exists" => Ok(FilterOp::NotExists),
        _ => Err(mlua::Error::RuntimeError(format!("unknown filter operator '{}'", op_name))),
    }
}

/// Convert a Lua data table to a HashMap<String, String> for create/update.
pub(crate) fn lua_table_to_hashmap(tbl: &mlua::Table) -> mlua::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;
        let s = match v {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            Value::Nil => continue,
            _ => continue,
        };
        map.insert(k, s);
    }
    Ok(map)
}

/// Convert a Document to a Lua table.
pub(crate) fn document_to_lua_table(lua: &Lua, doc: &crate::core::Document) -> mlua::Result<mlua::Table> {
    let tbl = lua.create_table()?;
    tbl.set("id", doc.id.as_str())?;
    for (k, v) in &doc.fields {
        tbl.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    if let Some(ref ts) = doc.created_at {
        tbl.set("created_at", ts.as_str())?;
    }
    if let Some(ref ts) = doc.updated_at {
        tbl.set("updated_at", ts.as_str())?;
    }
    Ok(tbl)
}

/// Convert a find result (documents + total) to a Lua table.
pub(crate) fn find_result_to_lua(lua: &Lua, docs: &[crate::core::Document], total: i64) -> mlua::Result<mlua::Table> {
    let tbl = lua.create_table()?;
    let docs_tbl = lua.create_table()?;
    for (i, doc) in docs.iter().enumerate() {
        docs_tbl.set(i + 1, document_to_lua_table(lua, doc)?)?;
    }
    tbl.set("documents", docs_tbl)?;
    tbl.set("total", total)?;
    Ok(tbl)
}
