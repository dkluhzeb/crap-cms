//! Core update operation for collections.

use crate::{
    db::{AccessResult, LocaleContext, query},
    hooks::{AccessCheckInput, HookContext, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, ServiceContext, WriteInput, WriteResult,
        persist_draft_version, persist_update, run_after_change_hooks,
    },
};

use super::ServiceError;
use crate::core::nest_group_fields;
use crate::service::helpers::{
    collect_api_hidden_field_names, enforce_access_constraints, validate_password_policy,
};

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

    // Canonicalize incoming data to nested groups up front (idempotent); the
    // whole pipeline sees one shape, the DB edge flattens to columns.
    input.data = nest_group_fields(&input.data, &def.fields);

    // Collection-level access check. The incoming data is exposed to the
    // access function as `ctx.data` so it can gate on what is being written.
    let access = write_hooks.check_access(
        &AccessCheckInput::builder("update", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .id(Some(id))
            .data(Some(&input.data))
            .locale(input.locale_ctx.map(LocaleContext::access_locale))
            .ui_locale(input.ui_locale.as_deref())
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // When the hook returned Constrained filters (e.g. "only rows where
    // author_id = me"), enforce the row-level match before writing.
    enforce_access_constraints(ctx, id, &access, "Update", false)?;

    // Authoritative password-policy enforcement (all surfaces): a weak password
    // on an auth-collection update is rejected here as a `password` field error.
    // An empty password means "no change" and is skipped. `ctx.password_policy`
    // falls back to the default policy, so this can never silently skip.
    validate_password_policy(
        def.is_auth_collection(),
        input.password,
        ctx.password_policy,
    )?;

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing (data-aware: each
    // `access.update` rule sees `ctx.data` = its level and `ctx.document` = the
    // full incoming document).
    write_hooks.strip_write_access_data(
        &def.fields,
        &mut input.data,
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
        "update",
    );

    let hook_data = input.data.clone();

    let hook_ctx = HookContext::builder(ctx.slug, "update")
        .data(hook_data)
        .document_id(id)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
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
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;

    let doc = if is_draft && def.has_versions() {
        persist_draft_version(ctx, id, &final_ctx.data, input.locale_ctx)?
    } else {
        let opts = PersistOptions::builder()
            .password(input.password)
            .locale_ctx(input.locale_ctx)
            .locale_config(input.locale_ctx.map(|c| &c.config))
            .build();

        persist_update(ctx, id, &final_ctx.to_value_map(), &opts)?
    };

    // Hydrate join fields (arrays, blocks, has-many) BEFORE after-change hooks so
    // they can react to nested array/blocks/has-many data, not just scalar columns.
    let mut doc = doc;

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
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(ctx.user)
            .ui_locale(ui_locale)
            .build(),
        conn,
    )?;

    // NOTE: auth-document edits invalidate a user's live-update streams, but
    // that is published POST-COMMIT by the orchestrators (`update_document_pool`
    // / `update_many_pool` and their conn variants) on the outer context that
    // carries the invalidation transport — mirroring `publish_mutation_event`.
    // Doing it here (inner ctx, pre-commit) was both a no-op and unsafe on
    // rollback.

    // Strip read-denied fields from the returned document, after the hooks have
    // seen the full doc.
    let access_locale = input.locale_ctx.map(LocaleContext::access_locale);
    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, access_locale);
    doc.strip_fields(&collect_api_hidden_field_names(&def.fields, ""));

    Ok((doc, after_ctx))
}
