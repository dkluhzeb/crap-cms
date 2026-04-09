//! Core per-document update for bulk operations (partial update, no password/draft).

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, query},
    hooks::{HookContext, ValidationCtx},
    service::{
        AfterChangeInput, WriteInput, WriteResult, build_hook_data, hooks::WriteHooks,
        persist_bulk_update, run_after_change_hooks,
    },
};

use super::{ServiceError, helpers::strip_denied_fields};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a single document in a bulk operation (partial update).
///
/// Runs the full lifecycle: access check → field stripping → before-write hooks →
/// partial persist → after-write hooks → hydrate → read-denied stripping.
/// Does NOT manage transactions — caller must open/commit.
#[allow(clippy::too_many_arguments)]
pub fn update_many_single_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    mut input: WriteInput<'_>,
    user: Option<&Document>,
    locale_config: &LocaleConfig,
) -> Result<WriteResult> {
    // Collection-level access check
    let access = write_hooks.check_access(def.access.update.as_deref(), user, Some(id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // Strip write-denied fields before hook processing
    let denied = write_hooks.field_write_denied(&def.fields, user, "update");
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);
    let hook_ctx = HookContext::builder(slug, "update")
        .data(hook_data)
        .user(user)
        .build();

    let val_ctx = ValidationCtx::builder(conn, slug)
        .exclude_id(Some(id))
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = persist_bulk_update(
        conn,
        slug,
        id,
        def,
        &final_data,
        &final_ctx.data,
        input.locale_ctx,
        locale_config,
    )?;

    let ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(slug, "update")
            .req_context(final_ctx.context)
            .user(user)
            .build(),
        conn,
    )?;

    // Hydrate + strip read-denied fields
    let mut doc = doc;
    query::hydrate_document(conn, slug, &def.fields, &mut doc, None, input.locale_ctx)?;

    let read_denied = write_hooks.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

    Ok((doc, ctx))
}
