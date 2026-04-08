//! Global document read with the full read lifecycle.

use crate::{
    core::{Document, collection::GlobalDefinition},
    db::{AccessResult, DbConnection, LocaleContext, query},
    hooks::lifecycle::AfterReadCtx,
    service::{ServiceError, hooks::ReadHooks},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Read a global document with the full read lifecycle.
///
/// Steps: before_read -> get_global -> field-level read strip -> after_read.
pub fn get_global_document(
    conn: &dyn DbConnection,
    hooks: &dyn ReadHooks,
    slug: &str,
    def: &GlobalDefinition,
    locale_ctx: Option<&LocaleContext>,
    user: Option<&Document>,
    ui_locale: Option<&str>,
) -> Result<Document> {
    let access = hooks.check_access(def.access.read.as_deref(), user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Read access denied".into()));
    }

    hooks.before_read(&def.hooks, slug, "get")?;

    let mut doc = query::get_global(conn, slug, def, locale_ctx)?;

    let denied = hooks.field_read_denied(&def.fields, user);
    for name in &denied {
        doc.fields.remove(name);
    }

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: slug,
        operation: "get",
        user,
        ui_locale,
    };

    Ok(hooks.after_read_one(&ar_ctx, doc))
}
