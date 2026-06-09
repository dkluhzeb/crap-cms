//! Core per-document update for bulk operations (partial update, no password/draft).

use crate::{
    config::LocaleConfig,
    db::{AccessResult, LocaleContext, query},
    hooks::{AccessCheckInput, HookContext, ValidationCtx},
    service::{
        AfterChangeInput, ServiceContext, WriteInput, WriteResult, persist_bulk_update,
        run_after_change_hooks,
    },
};

use super::{ServiceError, helpers::strip_denied_fields};
use crate::service::helpers::{collect_api_hidden_field_names, enforce_access_constraints};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a single document in a bulk operation (partial update).
///
/// Runs the full lifecycle: access check -> field stripping -> before-write hooks ->
/// partial persist -> hydrate -> after-write hooks -> read-denied stripping.
/// Does NOT manage transactions — caller must open/commit.
pub(crate) fn update_many_single_in_conn(
    ctx: &ServiceContext,
    id: &str,
    mut input: WriteInput<'_>,
    locale_config: &LocaleConfig,
) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    let access = write_hooks.check_access(&AccessCheckInput {
        access: def.access.update.as_ref(),
        user: ctx.user,
        id: Some(id),
        data: Some(&input.data),
        locale: input.locale_ctx.map(LocaleContext::access_locale),
        operation: "update",
        collection: ctx.slug,
        ui_locale: input.ui_locale.as_deref(),
    })?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // When the hook returned Constrained filters, enforce row-level match.
    enforce_access_constraints(ctx, id, &access, "Update", false)?;

    let denied = write_hooks.field_write_denied(
        &def.fields,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
        "update",
    );
    strip_denied_fields(&denied, &mut input.data);

    let hook_data = input.data.clone();
    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(hook_data)
        .document_id(id)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .exclude_id(Some(id))
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .collection_required_locales(def.required_locales.as_ref())
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_value_map(&def.fields);

    let mut doc = persist_bulk_update(ctx, id, &final_data, input.locale_ctx, locale_config)?;

    // Hydrate join fields BEFORE after-change hooks so they see nested data.
    query::hydrate_document(
        conn,
        ctx.slug,
        &def.fields,
        &mut doc,
        None,
        input.locale_ctx,
    )?;

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
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(input.ui_locale.as_deref())
            .build(),
        conn,
    )?;

    let mut read_denied = write_hooks.field_read_denied(
        &def.fields,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );
    read_denied.extend(collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok((doc, after_ctx))
}
