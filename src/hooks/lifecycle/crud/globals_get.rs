//! Registration of `crap.globals.get` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::{Document, SharedRegistry},
    db::{LocaleContext, query},
    hooks::lifecycle::{
        HookContext, HookEvent,
        access::check_field_read_access_with_lua,
        converters::document_to_lua_table,
        execution::{AfterReadCtx, apply_after_read_inner, run_hooks_inner},
    },
};

use super::{get_tx_conn, helpers::*};

/// Fire the before_read hook for a global.
fn fire_before_read(
    lua: &Lua,
    def: &crate::core::collection::GlobalDefinition,
    slug: &str,
    user: Option<&Document>,
    ui_locale: Option<&str>,
) -> mlua::Result<()> {
    let before_ctx = HookContext::builder(slug, "get")
        .user(user)
        .ui_locale(ui_locale)
        .build();
    run_hooks_inner(lua, &def.hooks, HookEvent::BeforeRead, before_ctx)
        .map_err(|e| RuntimeError(format!("before_read hook error: {e:#}")))?;
    Ok(())
}

/// Core logic for `crap.globals.get`.
fn globals_get_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    slug: String,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(reg, &slug)?;

    enforce_access(
        lua,
        override_access,
        def.access.read.as_deref(),
        None,
        &mut vec![],
        "Read access denied",
    )?;

    fire_before_read(lua, &def, &slug, user.as_ref(), ui_locale.as_deref())?;

    let mut doc = query::get_global(conn, &slug, &def, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("get_global error: {e:#}")))?;

    if !override_access {
        let denied = check_field_read_access_with_lua(lua, &def.fields, user.as_ref());
        for name in &denied {
            doc.fields.remove(name);
        }
    }

    // Run after_read hooks
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: &slug,
        operation: "get",
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
    };
    let doc = apply_after_read_inner(lua, &ar_ctx, doc);

    document_to_lua_table(lua, &doc)
}

/// Register `crap.globals.get(slug, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_get(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let get_fn = lua.create_function(move |lua, (slug, opts): (String, Option<Table>)| {
        globals_get_inner(lua, &registry, &lc, slug, opts)
    })?;
    table.set("get", get_fn)?;
    Ok(())
}
