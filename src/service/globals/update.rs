//! Global document update.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    core::{
        Document, DocumentFields, collection::GlobalDefinition, event::EventOperation,
        nest_group_fields,
    },
    db::{AccessResult, DbConnection, LocaleContext, query, query::helpers::global_table},
    hooks::{AccessCheckInput, HookContext, LuaCrudInfra, ValidationCtx},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceContext, ServiceError, WriteHooks, WriteInput,
        WriteResult, flush_queue, flush_verification_queue, helpers as svc_helpers,
        run_after_change_hooks,
        versions::{self, VersionSnapshotCtx},
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a global document.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn update_global_document(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    if ctx.pool.is_some() {
        update_global_pool(ctx, input)
    } else {
        update_global_conn(ctx, input)
    }
}

fn update_global_pool(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.global_def()?;
    let mut conn = pool.write().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let queue = Rc::new(RefCell::new(Vec::new()));

    // The verification queue exists for NESTED Lua creates: a hook running
    // inside this transaction that creates a verify-email auth document must
    // get its verification mail sent after commit — without the queue it was
    // silently dropped (only the create orchestrators used to carry one).
    let vqueue = Rc::new(RefCell::new(Vec::new()));

    let infra = LuaCrudInfra::from_ctx(ctx, Some(queue.clone()), Some(vqueue.clone()));

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::global(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .inherit_write_infra(ctx)
        .event_queue(queue.clone())
        .build();

    let result = update_global_in_conn(&inner_ctx, input)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Update, &result.0.id, &result.0.fields);
    flush_queue(ctx, &queue);
    flush_verification_queue(ctx, &vqueue);

    Ok(result)
}

fn update_global_conn(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let result = update_global_in_conn(ctx, input)?;

    ctx.clear_cache();

    ctx.publish_mutation_event(EventOperation::Update, &result.0.id, &result.0.fields);

    Ok(result)
}

/// Core logic for global update — accepts `ServiceContext` for hook abstraction.
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if persistence fails.
pub fn update_global_in_conn(
    ctx: &ServiceContext,
    mut input: WriteInput<'_>,
) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def()?;

    // Canonicalize incoming data to nested groups up front (idempotent).
    input.data = nest_group_fields(&input.data, &def.fields);

    check_global_update_access(
        ctx,
        write_hooks,
        def,
        Some(&input.data),
        input.locale_ctx.map(LocaleContext::access_locale),
        input.ui_locale.as_deref(),
    )?;

    let is_draft = input.draft && def.has_drafts();
    let gtable = global_table(ctx.slug);
    let ui_locale = input.ui_locale.as_deref();

    // Data-aware write strip (each `access.update` rule sees `ctx.data` = its
    // level and `ctx.document` = the full incoming document).
    write_hooks.strip_write_access_data(
        &def.fields,
        &mut input.data,
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
        "update",
    );

    let final_ctx =
        run_global_before_write_hooks(write_hooks, ctx, def, &input, &gtable, is_draft, ui_locale)?;

    let mut doc = persist_global_update(conn, ctx, def, &gtable, &final_ctx, &input, is_draft)?;

    // Hydrate join fields BEFORE after-change hooks so they see nested data.
    query::hydrate_document(conn, &gtable, &def.fields, &mut doc, None, input.locale_ctx)?;

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .locale(
                input
                    .locale_ctx
                    .map(LocaleContext::access_locale)
                    .map(String::from),
            )
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(ui_locale)
            .build(),
        conn,
    )?;

    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);
    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, access_locale);
    doc.strip_fields(&svc_helpers::collect_api_hidden_field_names(
        &def.fields,
        "",
    ));

    Ok((doc, after_ctx))
}

/// Enforce the global-update access check. Globals don't support
/// filter-based access — the `Constrained` variant is rejected so
/// access hooks have to be boolean (true/false on `ctx.user`).
pub(crate) fn check_global_update_access(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &GlobalDefinition,
    data: Option<&DocumentFields>,
    locale: Option<&str>,
    ui_locale: Option<&str>,
) -> Result<()> {
    let access = write_hooks.check_access(
        &AccessCheckInput::builder("update", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .data(data)
            .locale(locale)
            .ui_locale(ui_locale)
            .build(),
    )?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }
    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
            ctx.slug
        )));
    }
    Ok(())
}

/// Build the hook + validation contexts and run the before-write
/// hook chain. The returned `ReqContext` carries the (possibly
/// mutated) data forward into the persistence step.
fn run_global_before_write_hooks(
    write_hooks: &dyn WriteHooks,
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    input: &WriteInput<'_>,
    gtable: &str,
    is_draft: bool,
    ui_locale: Option<&str>,
) -> Result<HookContext> {
    let hook_data = input.data.clone();
    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(hook_data)
        .document_id("default")
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(ui_locale)
        .build();

    let conn = ctx.resolve_conn()?;
    let val_ctx = ValidationCtx::builder(conn.as_ref(), gtable)
        .exclude_id(Some("default"))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    Ok(write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?)
}

/// Either save a draft version (when `is_draft && has_versions`) or
/// run the published-update pipeline (column update + join tables +
/// ref-count delta + version snapshot if versioned).
fn persist_global_update(
    conn: &dyn DbConnection,
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    gtable: &str,
    final_ctx: &HookContext,
    input: &WriteInput<'_>,
    is_draft: bool,
) -> Result<Document> {
    if is_draft && def.has_versions() {
        let existing_doc = query::get_global(conn, ctx.slug, def, input.locale_ctx)?;
        versions::save_draft_version(&versions::SaveDraftArgs {
            conn,
            table: gtable,
            parent_id: "default",
            fields: &def.fields,
            versions: def.versions.as_ref(),
            existing_doc: &existing_doc,
            data: &final_ctx.data,
            locale_ctx: input.locale_ctx,
        })?;
        return Ok(existing_doc);
    }
    persist_global_published_update(conn, ctx, def, gtable, final_ctx, input)
}

/// Published-write path: snapshot the outgoing refs, write the row +
/// join tables, apply the ref-count delta, and (if the global is
/// versioned) capture a "published" snapshot.
fn persist_global_published_update(
    conn: &dyn DbConnection,
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    gtable: &str,
    final_ctx: &HookContext,
    input: &WriteInput<'_>,
) -> Result<Document> {
    let locale_cfg = input
        .locale_ctx
        .map(|lctx| lctx.config.clone())
        .unwrap_or_default();

    let old_refs = query::ref_count::snapshot_outgoing_refs(
        conn,
        gtable,
        "default",
        &def.fields,
        &locale_cfg,
    )?;

    let final_data = final_ctx.to_value_map();
    let doc = query::update_global(conn, ctx.slug, def, &final_data, input.locale_ctx)?;

    query::save_join_table_data(
        conn,
        gtable,
        &def.fields,
        "default",
        &final_ctx.data,
        input.locale_ctx,
    )?;

    query::ref_count::after_update(conn, gtable, "default", &def.fields, &locale_cfg, &old_refs)?;

    if def.has_versions() {
        let snap_ctx = VersionSnapshotCtx::builder(gtable, "default")
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .build();
        versions::create_version_snapshot(conn, &snap_ctx, "published", &doc)?;
    }

    Ok(doc)
}
