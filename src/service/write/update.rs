//! Core update operation for collections.

use crate::{
    db::{AccessResult, LocaleContext, query},
    hooks::{HookContext, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, ServiceContext, WriteInput, WriteResult,
        persist_draft_version, persist_update, run_after_change_hooks,
    },
};

use super::{ServiceError, helpers::strip_denied_fields};
use crate::service::helpers::{collect_api_hidden_field_names, enforce_access_constraints};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks -> persist -> after-write hooks.
/// Handles draft-only version saves when `input.draft` is true.
/// Does NOT manage transactions — caller must open/commit.
pub(crate) fn update_document_in_conn(
    ctx: &ServiceContext,
    id: &str,
    mut input: WriteInput<'_>,
) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // Collection-level access check
    let access = write_hooks.check_access(
        def.access.update.as_deref(),
        ctx.user,
        Some(id),
        None,
        input.locale_ctx.map(LocaleContext::access_locale),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // When the hook returned Constrained filters (e.g. "only rows where
    // author_id = me"), enforce the row-level match before writing.
    enforce_access_constraints(ctx, id, &access, "Update", false)?;

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing
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
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .exclude_id(Some(id))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .collection_required_locales(def.required_locales.as_ref())
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_value_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        persist_draft_version(ctx, id, &final_ctx.data, input.locale_ctx)?
    } else {
        let opts = PersistOptions::builder()
            .password(input.password)
            .locale_ctx(input.locale_ctx)
            .locale_config(input.locale_ctx.map(|c| &c.config))
            .build();

        persist_update(ctx, id, &final_data, &opts)?
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

    // Strip read-denied fields AFTER hydration
    let mut read_denied = write_hooks.field_read_denied(
        &def.fields,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );
    read_denied.extend(collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok((doc, after_ctx))
}
