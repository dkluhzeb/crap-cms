//! Global document read with the full read lifecycle.

use crate::{
    core::Document,
    db::{AccessResult, query},
    hooks::lifecycle::AfterReadCtx,
    service::{GetGlobalInput, ServiceContext, ServiceError, helpers},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Read a global document with the full read lifecycle.
///
/// Steps: before_read -> get_global -> field-level read strip -> after_read.
pub fn get_global_document(ctx: &ServiceContext, input: &GetGlobalInput) -> Result<Document> {
    let resolved = ctx.resolve_conn()?;
    let conn = resolved.as_ref();
    let hooks = ctx.read_hooks()?;
    let def = ctx.global_def();

    let access = hooks.check_access(def.access.read.as_deref(), ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
            ctx.slug
        )));
    }

    hooks.before_read(&def.hooks, ctx.slug, "get")?;

    let mut doc = query::get_global(conn, ctx.slug, def, input.locale_ctx)?;

    let mut denied = hooks.field_read_denied(&def.fields, ctx.user);
    denied.extend(helpers::collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&denied);

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: ctx.slug,
        operation: "get",
        user: ctx.user,
        ui_locale: input.ui_locale,
    };

    Ok(hooks.after_read_one(&ar_ctx, doc))
}
