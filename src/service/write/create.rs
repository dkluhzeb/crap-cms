//! Core create operation for collections.

use crate::{
    db::{AccessResult, query},
    hooks::{HookContext, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, ServiceContext, WriteInput, WriteResult, build_hook_data,
        persist_create, run_after_change_hooks,
    },
};

use super::{ServiceError, helpers::strip_denied_fields};
use crate::service::helpers::collect_api_hidden_field_names;

type Result<T> = std::result::Result<T, ServiceError>;

/// Create a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks -> persist -> after-write hooks.
/// Does NOT manage transactions — caller must open/commit.
pub fn create_document_core(
    ctx: &ServiceContext,
    mut input: WriteInput<'_>,
) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def();

    // Collection-level access check
    let access = write_hooks.check_access(def.access.create.as_deref(), ctx.user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Create access denied".into()));
    }

    // `Constrained` returns make no sense for create: there is no target row
    // to match against, and evaluating the filter against the incoming data
    // would conflate access control with validation. Operators should return
    // true/false based on `ctx.data` instead.
    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for '{}.create' returned a filter table; filter-table returns are only valid for update/delete/undelete/unpublish (where a target row exists). Return true/false based on the incoming 'data' in ctx.",
            ctx.slug
        )));
    }

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing
    let denied = write_hooks.field_write_denied(&def.fields, ctx.user, "create");
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);
    let hook_ctx = HookContext::builder(ctx.slug, "create")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let mut persist_builder = PersistOptions::builder()
        .password(input.password)
        .locale_ctx(input.locale_ctx)
        .draft(is_draft);

    if let Some(lctx) = input.locale_ctx {
        persist_builder = persist_builder.locale_config(&lctx.config);
    }

    let doc = persist_create(ctx, &final_data, &final_ctx.data, &persist_builder.build())?;

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "create")
            .locale(input.locale)
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(ui_locale)
            .build(),
        conn,
    )?;

    // Hydrate join fields (arrays, blocks, has-many) so the returned document is complete
    let mut doc = doc;

    query::hydrate_document(
        conn,
        ctx.slug,
        &def.fields,
        &mut doc,
        None,
        input.locale_ctx,
    )?;

    // Strip read-denied fields AFTER hydration (hydration can add join data for denied fields)
    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok((doc, after_ctx))
}
