//! Global document update.

use std::{cell::RefCell, rc::Rc};

use anyhow::Context as _;

use crate::{
    core::event::EventOperation,
    db::{AccessResult, query, query::helpers::global_table},
    hooks::{HookContext, ValidationCtx},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceContext, ServiceError, WriteInput, WriteResult,
        build_hook_data, flush_queue, helpers as svc_helpers, run_after_change_hooks,
        versions::{self, VersionSnapshotCtx},
        write::helpers::strip_denied_fields,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a global document within a single transaction.
#[cfg(not(tarpaulin_include))]
pub fn update_global_document(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<WriteResult> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.global_def();
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let queue = Rc::new(RefCell::new(Vec::new()));

    let inner_ctx = ServiceContext::global(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .cache(ctx.cache.clone())
        .event_transport(ctx.event_transport.clone())
        .event_queue(queue.clone())
        .build();

    let result = update_global_core(&inner_ctx, input)?;
    drop(inner_ctx);

    tx.commit().context("Commit transaction")?;

    ctx.clear_cache();

    ctx.publish_mutation_event(
        EventOperation::Update,
        &result.0.id,
        result.0.fields.clone(),
    );
    flush_queue(ctx, &queue);

    Ok(result)
}

/// Core logic for global update — accepts ServiceContext for hook abstraction.
pub fn update_global_core(ctx: &ServiceContext, mut input: WriteInput<'_>) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def();

    let access = write_hooks.check_access(def.access.update.as_deref(), ctx.user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
            ctx.slug
        )));
    }

    let is_draft = input.draft && def.has_drafts();
    let gtable = global_table(ctx.slug);
    let ui_locale = input.ui_locale.as_deref();

    let denied = write_hooks.field_write_denied(&def.fields, ctx.user, "update");
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);
    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, &gtable)
        .exclude_id(Some("default"))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        let existing_doc = query::get_global(conn, ctx.slug, def, input.locale_ctx)?;

        versions::save_draft_version(
            conn,
            &gtable,
            "default",
            &def.fields,
            def.versions.as_ref(),
            &existing_doc,
            &final_ctx.data,
        )?;
        existing_doc
    } else {
        let locale_cfg = input
            .locale_ctx
            .map(|lctx| lctx.config.clone())
            .unwrap_or_default();

        let old_refs = query::ref_count::snapshot_outgoing_refs(
            conn,
            &gtable,
            "default",
            &def.fields,
            &locale_cfg,
        )?;

        let doc = query::update_global(conn, ctx.slug, def, &final_data, input.locale_ctx)?;

        query::save_join_table_data(
            conn,
            &gtable,
            &def.fields,
            "default",
            &final_ctx.data,
            input.locale_ctx,
        )?;

        query::ref_count::after_update(
            conn,
            &gtable,
            "default",
            &def.fields,
            &locale_cfg,
            old_refs,
        )?;

        if def.has_versions() {
            let snap_ctx = VersionSnapshotCtx::builder(&gtable, "default")
                .fields(&def.fields)
                .versions(def.versions.as_ref())
                .has_drafts(def.has_drafts())
                .build();
            versions::create_version_snapshot(conn, &snap_ctx, "published", &doc)?;
        }

        doc
    };

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "update")
            .locale(input.locale)
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(ui_locale)
            .build(),
        conn,
    )?;

    let mut doc = doc;

    query::hydrate_document(conn, &gtable, &def.fields, &mut doc, None, input.locale_ctx)?;

    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(svc_helpers::collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok((doc, after_ctx))
}
