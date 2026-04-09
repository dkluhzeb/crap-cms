//! Core update operation for collections.

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, query},
    hooks::{HookContext, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, WriteInput, WriteResult, build_hook_data,
        hooks::WriteHooks, persist_draft_version, persist_update, run_after_change_hooks,
    },
};

use super::{ServiceError, helpers::strip_denied_fields};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks -> persist -> after-write hooks.
/// Handles draft-only version saves when `input.draft` is true.
/// Does NOT manage transactions — caller must open/commit.
pub fn update_document_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    // Collection-level access check
    let access = write_hooks.check_access(def.access.update.as_deref(), user, Some(id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing
    let denied = write_hooks.field_write_denied(&def.fields, user, "update");
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);
    let hook_ctx = HookContext::builder(slug, "update")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, slug)
        .exclude_id(Some(id))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        persist_draft_version(conn, slug, id, def, &final_ctx.data, input.locale_ctx)?
    } else {
        let mut update_builder = PersistOptions::builder()
            .password(input.password)
            .locale_ctx(input.locale_ctx);
        if let Some(lctx) = input.locale_ctx {
            update_builder = update_builder.locale_config(&lctx.config);
        }
        persist_update(
            conn,
            slug,
            id,
            def,
            &final_data,
            &final_ctx.data,
            &update_builder.build(),
        )?
    };

    let ctx = run_after_change_hooks(
        write_hooks,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(slug, "update")
            .locale(input.locale)
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(user)
            .ui_locale(ui_locale)
            .build(),
        conn,
    )?;

    // Hydrate join fields (arrays, blocks, has-many) so the returned document is complete
    let mut doc = doc;
    query::hydrate_document(conn, slug, &def.fields, &mut doc, None, input.locale_ctx)?;

    // Strip read-denied fields AFTER hydration
    let read_denied = write_hooks.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

    Ok((doc, ctx))
}
