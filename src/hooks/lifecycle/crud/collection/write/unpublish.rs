//! Unpublish operation — reverts a published document to draft status.

use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use anyhow::Result;

use crate::{
    core::{CollectionDefinition, Document, SharedRegistry},
    db::{DbConnection, query},
    hooks::{
        HookContext, HookEvent,
        lifecycle::{
            converters::document_to_lua_table,
            crud::{get_tx_conn, helpers::*},
            execution::run_hooks_inner,
        },
    },
    service::{ServiceContext, persist_unpublish},
};

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
    /// Create a builder with the required fields.
    pub(super) fn builder(
        collection: &'a str,
        id: &'a str,
        def: &'a CollectionDefinition,
    ) -> UnpublishCtxBuilder<'a> {
        UnpublishCtxBuilder::new(collection, id, def)
    }
}

/// Handle the unpublish code path: revert to draft, fire hooks, return document.
pub(super) fn handle_unpublish(
    lua: &Lua,
    conn: &dyn DbConnection,
    ctx: &UnpublishCtx,
) -> mlua::Result<Table> {
    // Internal hook lifecycle lookup — fetches doc state before unpublish, not a user-facing read.
    let existing_doc = query::find_by_id_raw(conn, ctx.collection, ctx.def, ctx.id, None, false)
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?
        .ok_or_else(|| {
            RuntimeError(format!(
                "Document {} not found in {}",
                ctx.id, ctx.collection
            ))
        })?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, ctx.run_hooks, ctx.collection, "update");

    if hooks_enabled {
        let before_ctx = HookContext::builder(ctx.collection, "update")
            .data(existing_doc.fields.clone())
            .draft(true)
            .locale(ctx.locale_str)
            .user(ctx.hook_user)
            .ui_locale(ctx.hook_ui_locale)
            .build();

        run_hooks_inner(lua, &ctx.def.hooks, HookEvent::BeforeChange, before_ctx)
            .map_err(|e| RuntimeError(format!("before_change hook error: {e:#}")))?;
    }

    let svc_ctx = ServiceContext::collection(ctx.collection, ctx.def)
        .conn(conn)
        .build();

    persist_unpublish(&svc_ctx, ctx.id)
        .map_err(|e| RuntimeError(format!("unpublish error: {e:#}")))?;

    // Internal hook lifecycle lookup — fetches doc state after unpublish, not a user-facing read.
    let updated_doc = query::find_by_id_raw(conn, ctx.collection, ctx.def, ctx.id, None, false)
        .map_err(|e| RuntimeError(format!("find error after unpublish: {e:#}")))?
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
            .map_err(|e| RuntimeError(format!("after_change hook error: {e:#}")))?;
    }

    document_to_lua_table(lua, &updated_doc)
}

/// Builder for [`UnpublishCtx`].
pub(super) struct UnpublishCtxBuilder<'a> {
    collection: &'a str,
    id: &'a str,
    def: &'a CollectionDefinition,
    run_hooks: bool,
    locale_str: Option<&'a str>,
    hook_user: Option<&'a Document>,
    hook_ui_locale: Option<&'a str>,
}

impl<'a> UnpublishCtxBuilder<'a> {
    pub(super) fn new(collection: &'a str, id: &'a str, def: &'a CollectionDefinition) -> Self {
        Self {
            collection,
            id,
            def,
            run_hooks: true,
            locale_str: None,
            hook_user: None,
            hook_ui_locale: None,
        }
    }

    pub(super) fn run_hooks(mut self, v: bool) -> Self {
        self.run_hooks = v;
        self
    }

    pub(super) fn locale_str(mut self, v: Option<&'a str>) -> Self {
        self.locale_str = v;
        self
    }

    pub(super) fn hook_user(mut self, v: Option<&'a Document>) -> Self {
        self.hook_user = v;
        self
    }

    pub(super) fn hook_ui_locale(mut self, v: Option<&'a str>) -> Self {
        self.hook_ui_locale = v;
        self
    }

    pub(super) fn build(self) -> UnpublishCtx<'a> {
        UnpublishCtx {
            collection: self.collection,
            id: self.id,
            def: self.def,
            run_hooks: self.run_hooks,
            locale_str: self.locale_str,
            hook_user: self.hook_user,
            hook_ui_locale: self.hook_ui_locale,
        }
    }
}

/// Standalone `crap.collections.unpublish(collection, id, opts?)`.
fn unpublish_document(
    lua: &Lua,
    reg: &SharedRegistry,
    collection: String,
    id: String,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_collection(reg, &collection)?;

    if !def.has_versions() {
        return Err(RuntimeError(format!(
            "Collection '{}' does not have versioning enabled",
            collection
        )));
    }

    // Access check — unpublish requires update access
    enforce_access(
        lua,
        &EnforceAccessParams {
            slug: &collection,
            override_access: false,
            access_fn: def.access.update.as_deref(),
            id: Some(&id),
            deny_msg: "Update access denied",
            injecting_status: false,
        },
        &mut vec![],
    )?;

    handle_unpublish(
        lua,
        conn,
        &UnpublishCtx::builder(&collection, &id, &def)
            .run_hooks(run_hooks)
            .hook_user(user.as_ref())
            .hook_ui_locale(ui_locale.as_deref())
            .build(),
    )
}

/// Register `crap.collections.unpublish(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_unpublish(lua: &Lua, table: &Table, registry: SharedRegistry) -> Result<()> {
    let unpublish_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            unpublish_document(lua, &registry, collection, id, opts)
        },
    )?;
    table.set("unpublish", unpublish_fn)?;

    Ok(())
}
