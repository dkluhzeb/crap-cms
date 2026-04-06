//! Registration of `crap.collections.update` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, SharedRegistry},
    db::{DbConnection, LocaleContext, query},
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
    service::{PersistOptions, persist_draft_version, persist_update},
};

use super::unpublish::{UnpublishCtx, handle_unpublish};
use super::{get_tx_conn, helpers::*};

/// Shared context for an update operation.
struct UpdateCtx<'a> {
    collection: &'a str,
    is_draft: bool,
    locale: Option<&'a str>,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
    override_access: bool,
}

/// Strip field-level write-denied fields from both data maps.
fn strip_write_denied_fields(
    lua: &Lua,
    ctx: &UpdateCtx<'_>,
    def: &CollectionDefinition,
    data: &mut HashMap<String, String>,
    hook: &mut HashMap<String, Value>,
) {
    if ctx.override_access {
        return;
    }
    let denied = check_field_write_access_with_lua(lua, &def.fields, ctx.user, "update");
    for name in &denied {
        data.remove(name);
        hook.remove(name);
    }
}

/// Run a field + collection hook phase (before_validate or before_change).
fn run_hook_phase(
    lua: &Lua,
    def: &CollectionDefinition,
    field_event: &FieldHookEvent,
    collection_event: HookEvent,
    hook: &mut HashMap<String, Value>,
    ctx: &UpdateCtx<'_>,
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
        .draft(ctx.is_draft)
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
    doc: &mut Document,
    ctx: &UpdateCtx<'_>,
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
        .draft(ctx.is_draft)
        .locale(ctx.locale)
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();

    run_hooks_inner(lua, &def.hooks, HookEvent::AfterChange, hook_ctx)
        .map_err(|e| RuntimeError(format!("after_change hook error: {e:#}")))?;

    Ok(())
}

/// Hydrate join-table fields and strip read-denied fields before returning.
fn hydrate_and_strip(
    lua: &Lua,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    doc: &mut Document,
    locale_ctx: Option<&LocaleContext>,
    ctx: &UpdateCtx<'_>,
) -> mlua::Result<Table> {
    query::hydrate_document(conn, ctx.collection, &def.fields, doc, None, locale_ctx)
        .map_err(|e| RuntimeError(format!("hydrate error: {e:#}")))?;

    if !ctx.override_access {
        let denied = check_field_read_access_with_lua(lua, &def.fields, ctx.user);
        for name in &denied {
            doc.fields.remove(name);
        }
    }

    document_to_lua_table(lua, doc)
}

/// Execute the `crap.collections.update` operation.
fn update_document(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    id: String,
    data_table: Table,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let unpublish = get_opt_bool(&opts, "unpublish", false)?;
    let draft = get_opt_bool(&opts, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;
    let is_draft = draft && def.has_drafts();

    enforce_access(
        lua,
        override_access,
        def.access.update.as_deref(),
        Some(&id),
        &mut vec![],
        "Update access denied",
    )?;

    // Handle unpublish early return
    if unpublish && def.has_versions() {
        return handle_unpublish(
            lua,
            conn,
            &UnpublishCtx::builder(&collection, &id, &def)
                .run_hooks(run_hooks)
                .locale_str(locale_str.as_deref())
                .hook_user(user.as_ref())
                .hook_ui_locale(ui_locale.as_deref())
                .build(),
        );
    }

    let ctx = UpdateCtx {
        collection: &collection,
        is_draft,
        locale: locale_str.as_deref(),
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
        override_access,
    };

    let ExtractedData {
        mut flat,
        mut hook,
        password,
    } = extract_data(lua, &data_table, &def)?;

    strip_write_denied_fields(lua, &ctx, &def, &mut flat, &mut hook);

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &collection, "update");

    if hooks_enabled {
        run_hook_phase(
            lua,
            &def,
            &FieldHookEvent::BeforeValidate,
            HookEvent::BeforeValidate,
            &mut hook,
            &ctx,
        )?;
    }

    if run_hooks {
        let r = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
        let val_ctx = ValidationCtx::builder(conn, &collection)
            .exclude_id(Some(&id))
            .draft(is_draft)
            .locale_ctx(locale_ctx.as_ref())
            .registry(&r)
            .soft_delete(def.soft_delete)
            .build();
        validate_fields_inner(lua, &def.fields, &hook, &val_ctx)
            .map_err(|e| RuntimeError(format!("validation error: {e:#}")))?;
    }

    if hooks_enabled {
        run_hook_phase(
            lua,
            &def,
            &FieldHookEvent::BeforeChange,
            HookEvent::BeforeChange,
            &mut hook,
            &ctx,
        )?;
    }

    let final_data = HookContext::builder(&collection, "update")
        .data(hook.clone())
        .build()
        .to_string_map(&def.fields);

    let mut doc = if is_draft && def.has_versions() {
        persist_draft_version(conn, &collection, &id, &def, &hook, locale_ctx.as_ref())
            .map_err(|e| RuntimeError(format!("draft version error: {e:#}")))?
    } else {
        persist_update(
            conn,
            &collection,
            &id,
            &def,
            &final_data,
            &hook,
            &PersistOptions::builder()
                .password(password.as_deref())
                .locale_ctx(locale_ctx.as_ref())
                .locale_config(lc)
                .build(),
        )
        .map_err(|e| RuntimeError(format!("update error: {e:#}")))?
    };

    if hooks_enabled {
        run_after_change_hooks(lua, &def, &mut doc, &ctx)?;
    }

    hydrate_and_strip(lua, conn, &def, &mut doc, locale_ctx.as_ref(), &ctx)
}

/// Register `crap.collections.update(collection, id, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_update(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
        move |lua, (collection, id, data_table, opts): (String, String, Table, Option<Table>)| {
            update_document(lua, &registry, &lc, collection, id, data_table, opts)
        },
    )?;

    table.set("update", update_fn)?;
    Ok(())
}
