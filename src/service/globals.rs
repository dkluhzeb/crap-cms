//! Global document update orchestration.

use anyhow::{Context as _, Result};

use crate::core::collection::GlobalDefinition;
use crate::core::document::Document;
use crate::db::DbPool;
use crate::db::query;
use crate::hooks::lifecycle::HookRunner;

use super::{WriteInput, WriteResult, build_before_ctx, build_hook_data, run_after_change_hooks};

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
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let global_table = format!("_global_{}", slug);

    let ui_locale = input.ui_locale.as_deref();
    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = build_before_ctx(
        slug,
        "update",
        hook_data,
        input.locale.clone(),
        is_draft,
        user,
        ui_locale,
    );
    let final_ctx = runner.run_before_write(
        &def.hooks,
        &def.fields,
        hook_ctx,
        &tx,
        &global_table,
        Some("default"),
        user,
        is_draft,
        ui_locale,
        input.locale_ctx,
    )?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        let existing_doc = query::get_global(&tx, slug, def, input.locale_ctx)?;
        crate::service::versions::save_draft_version(
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
            crate::service::versions::create_version_snapshot(
                &tx,
                &global_table,
                "default",
                &def.fields,
                def.versions.as_ref(),
                def.has_drafts(),
                "published",
                &doc,
            )?;
        }
        doc
    };

    let ctx = run_after_change_hooks(
        runner,
        &def.hooks,
        &def.fields,
        slug,
        "update",
        &doc,
        input.locale,
        is_draft,
        final_ctx.context,
        &tx,
        user,
        ui_locale,
    )?;

    tx.commit().context("Commit transaction")?;
    Ok((doc, ctx))
}
