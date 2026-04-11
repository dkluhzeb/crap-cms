//! Document validation without persistence.

use crate::{
    core::{Document, FieldDefinition, collection::Hooks},
    db::DbConnection,
    hooks::{HookContext, ValidationCtx},
    service::{WriteInput, build_hook_data, hooks::WriteHooks},
};

use super::{ServiceError, helpers::strip_denied_fields};

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
}

/// Validate a document without persisting — runs the full before-write pipeline
/// (field stripping, field hooks, validation, collection hooks) and returns.
///
/// Used by live validation endpoints.
pub fn validate_document(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    ctx: &ValidateContext<'_>,
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<()> {
    // Note: collection-level access check is intentionally skipped here.
    // Validation endpoints already check access before calling this function.

    let is_draft = input.draft;

    // Strip write-denied fields
    let denied = write_hooks.field_write_denied(ctx.fields, user, ctx.operation);
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);

    let hook_ctx = HookContext::builder(ctx.slug, ctx.operation)
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.table_name)
        .exclude_id(ctx.exclude_id)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(ctx.soft_delete)
        .build();

    write_hooks.run_before_write(ctx.hooks, ctx.fields, hook_ctx, &val_ctx)?;

    Ok(())
}
