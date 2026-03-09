//! Registration of `crap.collections.find`, `find_by_id`, and `count` Lua functions.

use anyhow::Result;
use mlua::{Lua, Value};
use crate::config::{LocaleConfig, PaginationConfig};
use crate::core::SharedRegistry;
use crate::core::upload;
use crate::db::query::{self, AccessResult, FindQuery, Filter, FilterOp, FilterClause, LocaleContext};
use crate::db::query::filter::normalize_filter_fields;

use super::get_tx_conn;
use crate::hooks::lifecycle::{UserContext, UiLocaleContext, HookContext, HookEvent};
use crate::hooks::lifecycle::execution::{run_hooks_inner, apply_after_read_inner};
use crate::hooks::lifecycle::access::{check_access_with_lua, check_field_read_access_with_lua};
use crate::hooks::lifecycle::converters::*;

/// Register `crap.collections.find(collection, query?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_find(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
    pagination_config: &PaginationConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let pg_default = pagination_config.default_limit;
    let pg_max = pagination_config.max_limit;
    let pg_cursor = pagination_config.is_cursor();
    let find_fn = lua.create_function(move |lua, (collection, query_table): (String, Option<mlua::Table>)| {
        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        let hook_user = lua.app_data_ref::<UserContext>().and_then(|uc| uc.0.clone());
        let hook_ui_locale = lua.app_data_ref::<UiLocaleContext>().and_then(|uc| uc.0.clone());

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

        let draft: bool = query_table.as_ref()
            .and_then(|qt| qt.get::<Option<bool>>("draft").ok().flatten())
            .unwrap_or(false);

        let (mut find_query, lua_page) = match query_table {
            Some(qt) => lua_table_to_find_query(&qt)?,
            None => (FindQuery::default(), None),
        };

        // Clamp limit to configured bounds
        find_query.limit = Some(query::apply_pagination_limits(
            find_query.limit, pg_default, pg_max,
        ));

        // Convert page -> offset if page was provided
        if let Some(p) = lua_page {
            let clamped = find_query.limit.unwrap_or(pg_default);
            find_query.offset = Some((p.max(1) - 1) * clamped);
        }

        // Ignore cursors if cursor pagination is disabled
        if !pg_cursor {
            find_query.after_cursor = None;
            find_query.before_cursor = None;
        }

        // Normalize dot notation: group dots -> __, array/block/rel dots preserved
        normalize_filter_fields(&mut find_query.filters, &def.fields);

        // Draft-aware filtering
        if def.has_drafts() && !draft {
            find_query.filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            }));
        }

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

        // Fire before_read hooks
        let before_ctx = HookContext::builder(&collection, "find")
            .user(hook_user.as_ref())
            .ui_locale(hook_ui_locale.as_deref())
            .build();
        run_hooks_inner(lua, &def.hooks, HookEvent::BeforeRead, before_ctx)
            .map_err(|e| mlua::Error::RuntimeError(format!("before_read hook error: {}", e)))?;

        query::validate_query_fields(&def, &find_query, locale_ctx.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;

        let mut docs = query::find(conn, &collection, &def, &find_query, locale_ctx.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;
        let total = query::count_with_search(conn, &collection, &def, &find_query.filters, locale_ctx.as_ref(), find_query.search.as_deref())
            .map_err(|e| mlua::Error::RuntimeError(format!("count error: {}", e)))?;

        // Hydrate join table data + populate relationships
        let select_slice = find_query.select.as_deref();
        for doc in &mut docs {
            query::hydrate_document(conn, &collection, &def.fields, doc, select_slice, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;
        }
        if depth > 0 {
            let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                format!("Registry lock: {}", e)
            ))?;
            let pop_ctx = query::PopulateContext {
                conn, registry: &r, collection_slug: &collection, def: &def,
            };
            let pop_opts = query::PopulateOpts {
                depth, select: select_slice, locale_ctx: locale_ctx.as_ref(),
            };
            query::populate_relationships_batch(
                &pop_ctx, &mut docs, &pop_opts,
            ).map_err(|e| mlua::Error::RuntimeError(format!("populate error: {}", e)))?;
        }
        // Assemble sizes for upload collections
        if let Some(ref upload_config) = def.upload {
            if upload_config.enabled {
                for doc in &mut docs {
                    upload::assemble_sizes_object(doc, upload_config);
                }
            }
        }

        // Apply select field stripping
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

        // Run after_read hooks
        let docs: Vec<_> = docs.into_iter()
            .map(|doc| apply_after_read_inner(lua, &def.hooks, &def.fields, &collection, "find", doc, hook_user.as_ref(), hook_ui_locale.as_deref()))
            .collect();

        let limit = find_query.limit.unwrap_or(pg_default);
        let offset: i64 = find_query.offset.unwrap_or(0);
        let page: i64 = lua_page.unwrap_or_else(|| if limit > 0 { offset / limit + 1 } else { 1 }).max(1);

        // Build pagination table (camelCase, PayloadCMS-style)
        let pagination = lua.create_table()?;
        pagination.set("totalDocs", total)?;
        pagination.set("limit", limit)?;

        if pg_cursor {
            let (sort_col, sort_dir) = if let Some(ref order) = find_query.order_by {
                if let Some(stripped) = order.strip_prefix('-') {
                    (stripped.to_string(), "DESC")
                } else {
                    (order.clone(), "ASC")
                }
            } else if def.timestamps {
                ("created_at".to_string(), "DESC")
            } else {
                ("id".to_string(), "ASC")
            };
            let (start_cursor, end_cursor) = query::cursor::build_cursors(
                &docs, &sort_col, sort_dir,
            );
            let using_before = find_query.before_cursor.is_some();
            let has_cursor = find_query.after_cursor.is_some() || using_before;
            let at_limit = docs.len() as i64 >= limit && !docs.is_empty();
            let (has_next, has_prev) = if using_before {
                (true, at_limit)
            } else {
                (at_limit, has_cursor)
            };
            pagination.set("hasNextPage", has_next)?;
            pagination.set("hasPrevPage", has_prev)?;
            if let Some(sc) = start_cursor {
                pagination.set("startCursor", sc)?;
            }
            if let Some(ec) = end_cursor {
                pagination.set("endCursor", ec)?;
            }
        } else {
            let total_pages = if limit > 0 { (total + limit - 1) / limit } else { 0 };
            pagination.set("totalPages", total_pages)?;
            pagination.set("page", page)?;
            pagination.set("pageStart", offset + 1)?;
            pagination.set("hasNextPage", page < total_pages)?;
            pagination.set("hasPrevPage", page > 1)?;
            if page > 1 {
                pagination.set("prevPage", page - 1)?;
            }
            if page < total_pages {
                pagination.set("nextPage", page + 1)?;
            }
        }

        let result = find_result_to_lua(lua, &docs, pagination)?;
        Ok(result)
    })?;
    table.set("find", find_fn)?;
    Ok(())
}

/// Register `crap.collections.find_by_id(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_find_by_id(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let find_by_id_fn = lua.create_function(move |lua, (collection, id, opts): (String, String, Option<mlua::Table>)| {
        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        let hook_user = lua.app_data_ref::<UserContext>().and_then(|uc| uc.0.clone());
        let hook_ui_locale = lua.app_data_ref::<UiLocaleContext>().and_then(|uc| uc.0.clone());

        let depth: i32 = opts.as_ref()
            .and_then(|o| o.get::<i32>("depth").ok())
            .unwrap_or(0)
            .clamp(0, 10);

        let locale_str: Option<String> = opts.as_ref()
            .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
        let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

        let use_draft: bool = opts.as_ref()
            .and_then(|o| o.get::<Option<bool>>("draft").ok().flatten())
            .unwrap_or(false);

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

        // Fire before_read hooks
        let before_ctx = HookContext::builder(&collection, "find_by_id")
            .user(hook_user.as_ref())
            .ui_locale(hook_ui_locale.as_deref())
            .build();
        run_hooks_inner(lua, &def.hooks, HookEvent::BeforeRead, before_ctx)
            .map_err(|e| mlua::Error::RuntimeError(format!("before_read hook error: {}", e)))?;

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

        // Unified find: draft overlay + constraints + hydration
        let mut doc = crate::db::ops::find_by_id_full(
            conn, &collection, &def, &id,
            locale_ctx.as_ref(), access_constraints, use_draft,
        ).map_err(|e| mlua::Error::RuntimeError(format!("find_by_id error: {}", e)))?;

        if let Some(ref mut d) = doc {
            let select_slice = select.as_deref();
            if depth > 0 {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                let mut visited = std::collections::HashSet::new();
                let pop_ctx = query::PopulateContext {
                    conn, registry: &r, collection_slug: &collection, def: &def,
                };
                let pop_opts = query::PopulateOpts {
                    depth, select: select_slice, locale_ctx: locale_ctx.as_ref(),
                };
                query::populate_relationships(
                    &pop_ctx, d, &mut visited, &pop_opts,
                ).map_err(|e| mlua::Error::RuntimeError(format!("populate error: {}", e)))?;
            }
            if let Some(ref upload_config) = def.upload {
                if upload_config.enabled {
                    upload::assemble_sizes_object(d, upload_config);
                }
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

        // Run after_read hooks
        let doc = doc.map(|d| apply_after_read_inner(lua, &def.hooks, &def.fields, &collection, "find_by_id", d, hook_user.as_ref(), hook_ui_locale.as_deref()));

        match doc {
            Some(d) => Ok(Value::Table(document_to_lua_table(lua, &d)?)),
            None => Ok(Value::Nil),
        }
    })?;
    table.set("find_by_id", find_by_id_fn)?;
    Ok(())
}

/// Register `crap.collections.count(collection, query?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_count(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
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

        let draft: bool = query_table.as_ref()
            .and_then(|qt| qt.get::<Option<bool>>("draft").ok().flatten())
            .unwrap_or(false);

        let (find_query, _) = match query_table {
            Some(ref qt) => (lua_table_to_find_query(qt)?.0, true),
            None => (query::FindQuery::default(), false),
        };
        let mut filters = find_query.filters;
        let search = find_query.search;

        normalize_filter_fields(&mut filters, &def.fields);

        if def.has_drafts() && !draft {
            filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            }));
        }

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

        let count = query::count_with_search(conn, &collection, &def, &filters, locale_ctx.as_ref(), search.as_deref())
            .map_err(|e| mlua::Error::RuntimeError(format!("count error: {}", e)))?;

        Ok(count)
    })?;
    table.set("count", count_fn)?;
    Ok(())
}
