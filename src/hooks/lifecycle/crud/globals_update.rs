//! Registration of `crap.globals.update` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{Document, SharedRegistry, collection::GlobalDefinition},
    db::{
        LocaleContext,
        query::{self, helpers::global_table},
    },
    hooks::{
        HookContext, HookEvent, ValidationCtx,
        lifecycle::{
            FieldHookEvent,
            access::{check_field_read_access_with_lua, check_field_write_access_with_lua},
            converters::*,
            execution::{run_field_hooks_inner, run_hooks_inner},
            validation::validate_fields_inner,
        },
    },
};

use super::{get_tx_conn, helpers::*};

/// Context for a global update operation.
struct GlobalUpdateCtx<'a> {
    slug: &'a str,
    locale: Option<&'a str>,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
    override_access: bool,
}

/// Run a field + collection hook phase (before_validate or before_change).
fn run_hook_phase(
    lua: &Lua,
    def: &GlobalDefinition,
    field_event: &FieldHookEvent,
    collection_event: HookEvent,
    hook: &mut HashMap<String, Value>,
    ctx: &GlobalUpdateCtx<'_>,
) -> mlua::Result<()> {
    let label = format!("{field_event:?}");

    run_field_hooks_inner(lua, &def.fields, field_event, hook, ctx.slug, "update")
        .map_err(|e| RuntimeError(format!("{label} field hook error: {e:#}")))?;

    let hook_ctx = HookContext::builder(ctx.slug, "update")
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
    def: &GlobalDefinition,
    doc: &Document,
    ctx: &GlobalUpdateCtx<'_>,
) -> mlua::Result<()> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));

    run_field_hooks_inner(
        lua,
        &def.fields,
        &FieldHookEvent::AfterChange,
        &mut after_data,
        ctx.slug,
        "update",
    )
    .map_err(|e| RuntimeError(format!("after_change field hook error: {e:#}")))?;

    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(after_data)
        .locale(ctx.locale)
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();

    run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, hook_ctx)
        .map_err(|e| RuntimeError(format!("after_change hook error: {e:#}")))?;

    Ok(())
}

/// Core logic for `crap.globals.update`.
fn globals_update_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    slug: String,
    data_table: Table,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(reg, &slug)?;

    enforce_access(
        lua,
        override_access,
        def.access.update.as_deref(),
        None,
        &mut vec![],
        "Update access denied",
    )?;

    let ctx = GlobalUpdateCtx {
        slug: &slug,
        locale: locale_str.as_deref(),
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
        override_access,
    };

    let mut data = lua_table_to_hashmap(&data_table)?;
    let mut hook_data = lua_table_to_json_map(lua, &data_table)?;

    // Merge flat data into hook_data (JSON values for hooks to see)
    for (k, v) in &data {
        hook_data
            .entry(k.clone())
            .or_insert_with(|| Value::String(v.clone()));
    }

    if !ctx.override_access {
        let denied = check_field_write_access_with_lua(lua, &def.fields, ctx.user, "update");
        for name in &denied {
            data.remove(name);
            hook_data.remove(name);
        }
    }

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &slug, "update");

    if hooks_enabled {
        run_hook_phase(
            lua,
            &def,
            &FieldHookEvent::BeforeValidate,
            HookEvent::BeforeValidate,
            &mut hook_data,
            &ctx,
        )?;
    }

    if run_hooks {
        let gtable = global_table(&slug);
        let val_ctx = ValidationCtx::builder(conn, &gtable)
            .locale_ctx(locale_ctx.as_ref())
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

    // Convert hook-modified data to string map for DB write
    let final_data = HookContext::builder(&slug, "update")
        .data(hook_data.clone())
        .build()
        .to_string_map(&def.fields);

    let gtable = global_table(&slug);

    let old_refs =
        query::ref_count::snapshot_outgoing_refs(conn, &gtable, "default", &def.fields, lc)
            .map_err(|e| RuntimeError(format!("ref count snapshot error: {e:#}")))?;

    query::update_global(conn, &slug, &def, &final_data, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("update_global error: {e:#}")))?;

    query::save_join_table_data(
        conn,
        &gtable,
        &def.fields,
        "default",
        &hook_data,
        locale_ctx.as_ref(),
    )
    .map_err(|e| RuntimeError(format!("join data error: {e:#}")))?;

    query::ref_count::after_update(conn, &gtable, "default", &def.fields, lc, old_refs)
        .map_err(|e| RuntimeError(format!("ref count update error: {e:#}")))?;

    // Re-fetch to hydrate join data in the returned document
    let doc = query::get_global(conn, &slug, &def, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("get_global error: {e:#}")))?;

    if hooks_enabled {
        run_after_change_hooks(lua, &def, &doc, &ctx)?;
    }

    let mut doc = doc;

    if !ctx.override_access {
        let denied = check_field_read_access_with_lua(lua, &def.fields, ctx.user);
        for name in &denied {
            doc.fields.remove(name);
        }
    }

    document_to_lua_table(lua, &doc)
}

/// Register `crap.globals.update(slug, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_update(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
        move |lua, (slug, data_table, opts): (String, Table, Option<Table>)| {
            globals_update_inner(lua, &registry, &lc, slug, data_table, opts)
        },
    )?;
    table.set("update", update_fn)?;
    Ok(())
}
