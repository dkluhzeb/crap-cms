//! Global document update.

use anyhow::Context as _;

use crate::{
    core::{Document, collection::GlobalDefinition},
    db::{DbPool, query, query::helpers::global_table},
    hooks::{HookContext, HookRunner, ValidationCtx},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceError, WriteInput, WriteResult, build_hook_data,
        run_after_change_hooks,
        versions::{self, VersionSnapshotCtx},
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a global document within a single transaction: before-hooks -> update -> after-hooks.
/// When `draft` is true and the global has drafts enabled, creates a version-only save.
#[cfg(not(tarpaulin_include))]
pub fn update_global_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &GlobalDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;
    let wh = RunnerWriteHooks {
        runner,
        hooks_enabled: true,
        conn: Some(&tx),
    };
    let result = update_global_core(&tx, &wh, slug, def, input, user)?;
    tx.commit().context("Commit transaction")?;
    Ok(result)
}

/// Core logic for global update -- accepts `&dyn WriteHooks` for hook abstraction.
pub fn update_global_core(
    conn: &dyn crate::db::DbConnection,
    write_hooks: &dyn crate::service::hooks::WriteHooks,
    slug: &str,
    def: &GlobalDefinition,
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    // Collection-level access check
    let access = write_hooks.check_access(def.access.update.as_deref(), user, None, None)?;
    if matches!(access, crate::db::AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let is_draft = input.draft && def.has_drafts();
    let gtable = global_table(slug);
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing
    let denied = write_hooks.field_write_denied(&def.fields, user, "update");
    let join_data = crate::service::write::helpers::strip_denied_fields(
        &denied,
        &mut input.data,
        input.join_data,
    );

    let hook_data = build_hook_data(&input.data, &join_data);
    let hook_ctx = HookContext::builder(slug, "update")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, &gtable)
        .exclude_id(Some("default"))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        let existing_doc = query::get_global(conn, slug, def, input.locale_ctx)?;
        versions::save_draft_version(
            conn,
            &gtable,
            "default",
            &def.fields,
            def.versions.as_ref(),
            &existing_doc,
            &final_ctx.data,
        )?;
        existing_doc
    } else {
        let locale_cfg = input
            .locale_ctx
            .map(|lctx| lctx.config.clone())
            .unwrap_or_default();
        let old_refs = query::ref_count::snapshot_outgoing_refs(
            conn,
            &gtable,
            "default",
            &def.fields,
            &locale_cfg,
        )?;

        let doc = query::update_global(conn, slug, def, &final_data, input.locale_ctx)?;
        query::save_join_table_data(
            conn,
            &gtable,
            &def.fields,
            "default",
            &final_ctx.data,
            input.locale_ctx,
        )?;
        query::ref_count::after_update(
            conn,
            &gtable,
            "default",
            &def.fields,
            &locale_cfg,
            old_refs,
        )?;

        if def.has_versions() {
            let ctx = VersionSnapshotCtx::builder(&gtable, "default")
                .fields(&def.fields)
                .versions(def.versions.as_ref())
                .has_drafts(def.has_drafts())
                .build();
            versions::create_version_snapshot(conn, &ctx, "published", &doc)?;
        }
        doc
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

    // Hydrate join fields so the returned document is complete
    let mut doc = doc;
    query::hydrate_document(conn, &gtable, &def.fields, &mut doc, None, input.locale_ctx)?;

    // Strip read-denied fields AFTER hydration
    let read_denied = write_hooks.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

    Ok((doc, ctx))
}
