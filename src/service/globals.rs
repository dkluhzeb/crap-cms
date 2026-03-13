//! Global document update orchestration.

use anyhow::{Context as _, Result};

use rusqlite::TransactionBehavior;

use crate::{
    core::{collection::GlobalDefinition, document::Document},
    db::{DbPool, query},
    hooks::lifecycle::{HookContext, HookRunner, ValidationCtx},
};

use super::{
    AfterChangeInput, WriteInput, WriteResult, build_hook_data, run_after_change_hooks,
    versions::{self, VersionSnapshotCtx},
};

/// Update a global document within a single transaction: before-hooks → update → after-hooks.
/// When `draft` is true and the global has drafts enabled, creates a version-only save.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn update_global_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &GlobalDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let is_draft = input.draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let global_table = format!("_global_{}", slug);

    let ui_locale = input.ui_locale.as_deref();
    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = HookContext::builder(slug, "update")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale)
        .build();
    let val_ctx = ValidationCtx::builder(&tx, &global_table)
        .exclude_id(Some("default"))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .build();
    let final_ctx = runner.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        let existing_doc = query::get_global(&tx, slug, def, input.locale_ctx)?;
        versions::save_draft_version(
            &tx,
            &global_table,
            "default",
            &def.fields,
            def.versions.as_ref(),
            &existing_doc,
            &final_ctx.data,
        )?;
        existing_doc
    } else {
        let doc = query::update_global(&tx, slug, def, &final_data, input.locale_ctx)?;
        query::save_join_table_data(
            &tx,
            &global_table,
            &def.fields,
            "default",
            &final_ctx.data,
            input.locale_ctx,
        )?;

        if def.has_versions() {
            let ctx = VersionSnapshotCtx::builder(&global_table, "default")
                .fields(&def.fields)
                .versions(def.versions.as_ref())
                .has_drafts(def.has_drafts())
                .build();
            versions::create_version_snapshot(&tx, &ctx, "published", &doc)?;
        }
        doc
    };

    let ctx = run_after_change_hooks(
        runner,
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
        &tx,
    )?;

    tx.commit().context("Commit transaction")?;
    Ok((doc, ctx))
}
