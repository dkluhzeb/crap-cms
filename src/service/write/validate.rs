//! Document validation without persistence.

use crate::{
    core::{Document, FieldDefinition, RequiredLocales, collection::Hooks, nest_group_fields},
    db::{DbConnection, LocaleContext},
    hooks::{HookContext, ValidationCtx},
    service::{WriteInput, hooks::WriteHooks},
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// Context for a validate-only run (no persist).
pub struct ValidateContext<'a> {
    pub slug: &'a str,
    /// Table name for unique checks — collection slug or `_global_{slug}`.
    pub table_name: &'a str,
    pub fields: &'a [FieldDefinition],
    pub hooks: &'a Hooks,
    pub operation: &'a str,
    /// Exclude this document from unique checks (update path).
    pub exclude_id: Option<&'a str>,
    pub soft_delete: bool,
    /// Whether the collection/global supports drafts (`has_drafts()`). The
    /// dry-run clamps `draft && supports_drafts` here — the one chokepoint —
    /// mirroring the real write path, so `draft=true` on a draft-disabled
    /// collection can't relax required-field checks on one surface but not
    /// another.
    pub supports_drafts: bool,
    /// Collection-level `required_locales` default, so the dry-run mirrors the
    /// completeness check that `create`/`update` apply. `None` for globals.
    pub required_locales: Option<&'a RequiredLocales>,
}

/// Validate a document without persisting — runs the full before-write pipeline
/// (field stripping, field hooks, validation, collection hooks) and returns.
///
/// Used by live validation endpoints.
///
/// # Errors
///
/// Returns service-layer errors (validation failures, hook errors) without
/// touching the database.
pub fn validate_document(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    ctx: &ValidateContext<'_>,
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<()> {
    // Note: collection-level access check is intentionally skipped here.
    // Validation endpoints already check access before calling this function.

    // Canonicalize incoming data to nested groups up front (idempotent) so the
    // dry-run pipeline matches the real write path.
    input.data = nest_group_fields(&input.data, ctx.fields);

    let is_draft = input.draft && ctx.supports_drafts;

    // Strip write-denied fields (data-aware: each `access.create`/`access.update`
    // rule sees `ctx.data` = its level and `ctx.document` = the incoming document).
    write_hooks.strip_write_access_data(
        ctx.fields,
        &mut input.data,
        ctx.slug,
        user,
        input.locale_ctx.map(LocaleContext::access_locale),
        ctx.operation,
    );

    let hook_data = input.data.clone();

    let mut hook_ctx_builder = HookContext::builder(ctx.slug, ctx.operation)
        .data(hook_data)
        .locale(input.locale_ctx.map(LocaleContext::access_locale))
        .draft(is_draft)
        .user(user);
    // On an update dry-run, expose the target id (matches the real write path,
    // so a field hook's `ctx.id` agrees between validate and persist).
    if let Some(id) = ctx.exclude_id {
        hook_ctx_builder = hook_ctx_builder.document_id(id);
    }
    let hook_ctx = hook_ctx_builder.build();

    let val_ctx = ValidationCtx::builder(conn, ctx.table_name)
        .exclude_id(ctx.exclude_id)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(ctx.soft_delete)
        .collection_required_locales(ctx.required_locales)
        .user(user)
        .ui_locale(input.ui_locale.as_deref())
        .build();

    write_hooks.run_before_write(ctx.hooks, ctx.fields, hook_ctx, &val_ctx)?;

    Ok(())
}
