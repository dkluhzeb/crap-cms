//! Registration of `crap.collections.update_many` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, SharedRegistry},
    db::{
        DbConnection, FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::{
        HookContext, HookEvent, ValidationCtx,
        lifecycle::{
            FieldHookEvent,
            access::check_field_write_access_with_lua,
            converters::{lua_table_to_find_query, lua_table_to_hashmap, lua_table_to_json_map},
            execution::{run_field_hooks_inner, run_hooks_inner},
            validation::validate_fields_inner,
        },
    },
};

use super::{get_tx_conn, helpers::*};

/// Context for a bulk update operation.
struct UpdateManyCtx<'a> {
    collection: &'a str,
    locale: Option<&'a str>,
    locale_ctx: Option<&'a LocaleContext>,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

/// Run a field + collection hook phase (before_validate or before_change).
fn run_hook_phase(
    lua: &Lua,
    def: &CollectionDefinition,
    field_event: &FieldHookEvent,
    collection_event: HookEvent,
    hook: &mut HashMap<String, Value>,
    ctx: &UpdateManyCtx<'_>,
) -> mlua::Result<()> {
    let label = format!("{field_event:?}");

    run_field_hooks_inner(
        lua,
        &def.fields,
        field_event,
        hook,
        ctx.collection,
        "update",
    )
    .map_err(|e| RuntimeError(format!("{label} field hook error: {e:#}")))?;

    let hook_ctx = HookContext::builder(ctx.collection, "update")
        .data(hook.clone())
        .locale(ctx.locale)
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();
    let result = run_hooks_inner(lua, &def.hooks, collection_event, hook_ctx)
        .map_err(|e| RuntimeError(format!("{label} hook error: {e:#}")))?;

    *hook = result.data;

    Ok(())
}

/// Run after_change field + collection hooks.
fn run_after_change_hooks(
    lua: &Lua,
    def: &CollectionDefinition,
    doc: &Document,
    ctx: &UpdateManyCtx<'_>,
) -> mlua::Result<()> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));

    run_field_hooks_inner(
        lua,
        &def.fields,
        &FieldHookEvent::AfterChange,
        &mut after_data,
        ctx.collection,
        "update",
    )
    .map_err(|e| RuntimeError(format!("after_change field hook error: {e:#}")))?;

    let hook_ctx = HookContext::builder(ctx.collection, "update")
        .data(after_data)
        .locale(ctx.locale)
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();
    run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, hook_ctx)
        .map_err(|e| RuntimeError(format!("after_change hook error: {e:#}")))?;

    Ok(())
}

/// Persist a single document update via the shared service layer function.
fn persist_single_update(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    ctx: &UpdateManyCtx<'_>,
    doc_id: &str,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, Value>,
    lc: &LocaleConfig,
) -> mlua::Result<Document> {
    crate::service::persist_bulk_update(
        conn,
        ctx.collection,
        doc_id,
        def,
        final_data,
        hook_data,
        ctx.locale_ctx,
        lc,
    )
    .map_err(|e| RuntimeError(format!("update error: {e:#}")))
}

/// Update multiple documents matching a query with the given data.
///
/// Runs the full per-document lifecycle for each matched document:
/// field hooks, validation, before/after change hooks, DB write,
/// ref count updates, FTS sync, and version snapshots.
fn update_many_documents(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: &str,
    query_table: &Table,
    data_table: &Table,
    opts: &Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let locale_str = get_opt_string(opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(opts, "hooks", true)?;
    let draft = get_opt_bool(opts, "draft", false)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_collection(reg, collection)?;

    let ctx = UpdateManyCtx {
        collection,
        locale: locale_str.as_deref(),
        locale_ctx: locale_ctx.as_ref(),
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
    };

    // Find matching documents
    let (mut find_query, _) = lua_table_to_find_query(query_table)?;
    normalize_filter_fields(&mut find_query.filters, &def.fields);
    add_draft_filter(&def, draft, &mut find_query.filters);

    enforce_access(
        lua,
        override_access,
        def.access.update.as_deref(),
        None,
        &mut find_query.filters,
        "Update access denied",
    )?;

    let mut find_all = FindQuery::new();
    find_all.filters = find_query.filters;
    let docs = query::find(conn, collection, &def, &find_all, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?;

    // Check per-doc update access (all-or-nothing)
    for doc in &docs {
        enforce_access(
            lua,
            override_access,
            def.access.update.as_deref(),
            Some(&doc.id),
            &mut vec![],
            &format!("Update access denied for document {}", doc.id),
        )?;
    }

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, collection, "update_many");

    let mut data = lua_table_to_hashmap(data_table)?;

    if def.is_auth_collection() && data.contains_key("password") {
        return Err(RuntimeError(
            "Cannot set password via update_many. Use single update instead.".into(),
        ));
    }

    let join_data = lua_table_to_json_map(lua, data_table)?;

    let mut base_hook_data: HashMap<String, Value> = data
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    base_hook_data.extend(join_data);

    if !override_access {
        let denied = check_field_write_access_with_lua(lua, &def.fields, user.as_ref(), "update");
        for name in &denied {
            data.remove(name);
            base_hook_data.remove(name);
        }
    }

    let mut modified = 0i64;

    for doc in &docs {
        let mut hook_data = base_hook_data.clone();

        if hooks_enabled {
            run_hook_phase(
                lua,
                &def,
                &FieldHookEvent::BeforeValidate,
                HookEvent::BeforeValidate,
                &mut hook_data,
                &ctx,
            )?;

            // Richtext node attr before_validate (parity with service layer)
            let r_lock = reg
                .read()
                .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
            crate::service::write_hooks::apply_richtext_before_validate(
                lua,
                &def.fields,
                &mut hook_data,
                &r_lock,
                collection,
            );
            drop(r_lock);
        }

        if run_hooks {
            let r = reg
                .read()
                .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
            let val_ctx = ValidationCtx::builder(conn, collection)
                .exclude_id(Some(&doc.id))
                .locale_ctx(ctx.locale_ctx)
                .soft_delete(def.soft_delete)
                .draft(draft)
                .registry(&r)
                .build();
            validate_fields_inner(lua, &def.fields, &hook_data, &val_ctx)
                .map_err(|e| RuntimeError(format!("validation error: {e:#}")))?;
        }

        if hooks_enabled {
            run_hook_phase(
                lua,
                &def,
                &FieldHookEvent::BeforeChange,
                HookEvent::BeforeChange,
                &mut hook_data,
                &ctx,
            )?;
        }

        let final_data = HookContext::builder(collection, "update")
            .data(hook_data.clone())
            .build()
            .to_string_map(&def.fields);

        let updated =
            persist_single_update(conn, &def, &ctx, &doc.id, &final_data, &hook_data, lc)?;

        if hooks_enabled {
            run_after_change_hooks(lua, &def, &updated, &ctx)?;
        }

        modified += 1;
    }

    let result = lua.create_table()?;
    result.set("modified", modified)?;
    Ok(result)
}

/// Register `crap.collections.update_many(collection, query, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_update_many(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let update_many_fn = lua.create_function(
        move |lua,
              (collection, query_table, data_table, opts): (
            String,
            Table,
            Table,
            Option<Table>,
        )| {
            update_many_documents(
                lua,
                &registry,
                &lc,
                &collection,
                &query_table,
                &data_table,
                &opts,
            )
        },
    )?;

    table.set("update_many", update_many_fn)?;

    Ok(())
}
