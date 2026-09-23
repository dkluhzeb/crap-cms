//! Core create operation for collections.

use crate::{
    db::{AccessResult, LocaleContext},
    hooks::{AccessCheckInput, HookContext, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, ServiceContext, WriteInput, WriteResult, persist_create,
        run_after_change_hooks,
        write::{UploadSettle, admit_create_input, settle_upload_write},
    },
};

use super::ServiceError;
use crate::service::helpers::{
    EmptyPassword, hydrate_reported, strip_reported, validate_password_policy,
};
use crate::service::hooks::WriteHooks;

type Result<T> = std::result::Result<T, ServiceError>;

/// The collection-level `create` access gate — ONE chokepoint shared by the
/// real create and the create-mode dry-run ([`op::Validate`]), so the two can
/// never drift. Callers pass canonicalized (group-nested) data — the access
/// function sees it as `ctx.data`.
///
/// `Constrained` returns make no sense for create: there is no target row to
/// match against, and evaluating the filter against the incoming data would
/// conflate access control with validation — operators should return
/// true/false based on `ctx.data` instead.
///
/// [`op::Validate`]: crate::service::op::Validate
pub(crate) fn check_create_access(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    def: &crate::core::CollectionDefinition,
    data: &crate::core::DocumentFields,
    locale: Option<&str>,
    ui_locale: Option<&str>,
) -> Result<()> {
    let access = write_hooks.check_access(
        &AccessCheckInput::builder("create", ctx.slug)
            .access(def.access.create.as_ref())
            .user(ctx.user)
            .data(Some(data))
            .locale(locale)
            .ui_locale(ui_locale)
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Create access denied".into()));
    }

    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for '{}.create' returned a filter table; filter-table returns are only valid for update/delete/undelete/unpublish (where a target row exists). Return true/false based on the incoming 'data' in ctx.",
            ctx.slug
        )));
    }

    Ok(())
}

/// Create a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks -> persist -> after-write hooks.
/// Does NOT manage transactions — caller must open/commit.
///
/// # Errors
///
/// Returns service-layer errors (access denied, validation, hook errors) or
/// a backend error if persistence fails.
pub fn create_document_in_conn(
    ctx: &ServiceContext,
    mut input: WriteInput<'_>,
) -> Result<WriteResult> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    // The admission prefix the `validate` dry-run runs too: canonicalize the
    // incoming data to the nested group shape (every surface and the whole
    // pipeline sees one shape; the DB edge flattens back to columns), strip
    // untrusted upload metadata, and refuse a non-default locale.
    admit_create_input(def, &mut input)?;

    check_create_access(
        ctx,
        write_hooks,
        def,
        &input.data,
        input.locale_ctx.map(LocaleContext::access_locale),
        input.ui_locale.as_deref(),
    )?;

    // Authoritative password-policy enforcement — one chokepoint for every
    // surface and every create path (single AND `create_many`); falls back to
    // the default policy so it can never silently skip. A present-but-empty
    // password is a caller error here: on create there is nothing to leave
    // alone, so treating it as "no change" would quietly produce a
    // passwordless account. See `validate_password_policy`.
    validate_password_policy(
        def.is_auth_collection(),
        input.password,
        ctx.password_policy,
        EmptyPassword::IsRejected,
    )?;

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing (data-aware: each
    // `access.create` rule sees `ctx.data` = its level and `ctx.document` = the
    // full incoming document — no row exists yet).
    write_hooks.strip_write_access_create(
        &def.fields,
        &mut input.data,
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );

    let hook_ctx = HookContext::builder(ctx.slug, "create")
        .data(input.data.clone())
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .user(ctx.user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .collection_required_locales(def.required_locales.as_ref())
        .user(ctx.user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_value_map();

    let opts = PersistOptions::builder()
        .password(input.password)
        .locale_ctx(input.locale_ctx)
        .locale_config(input.locale_ctx.map(|c| &c.config))
        .draft(is_draft)
        .build();

    let mut doc = persist_create(ctx, &final_data, &opts)?;

    // The new file's conversions go into the queue on THIS connection, inside
    // the write transaction — a create has no previous file to clean up.
    settle_upload_write(
        ctx,
        &UploadSettle::builder(def, &doc.id)
            .conversions(input.upload_conversions.as_ref())
            .build(),
    )?;

    // Hydrate join fields (arrays, blocks, has-many) BEFORE after-change hooks so
    // they can react to nested array/blocks/has-many data, not just scalar columns.
    hydrate_reported(ctx, &mut doc, input.locale_ctx)?;

    let after_ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(ctx.slug, "create")
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

    // Strip read-denied fields from the returned document, after the hooks have
    // seen the full doc (hydration can add join data for denied fields).
    strip_reported(ctx, write_hooks, &mut doc, input.locale_ctx)?;

    Ok((doc, after_ctx))
}
