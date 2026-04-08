//! Global document unpublish.

use anyhow::Context as _;

use crate::{
    core::{Document, collection::GlobalDefinition},
    db::{AccessResult, DbPool, query, query::helpers::global_table},
    hooks::{HookContext, HookEvent, HookRunner},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceError, WriteHooks, run_after_change_hooks,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a global document: access check → before-hooks → unpublish →
/// after-hooks → hydrate → strip read-denied fields → commit.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_global_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &GlobalDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let wh = RunnerWriteHooks {
        runner,
        hooks_enabled: true,
        conn: Some(&tx),
    };

    // Access check — unpublish requires update access
    let access = wh.check_access(def.access.update.as_deref(), user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let gtable = global_table(slug);
    let doc = query::get_global(&tx, slug, def, None)?;

    let hook_ctx = HookContext::builder(slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

    crate::service::unpublish_with_snapshot(
        &tx,
        &gtable,
        "default",
        &def.fields,
        def.versions.as_ref(),
        &doc,
    )?;

    run_after_change_hooks(
        &wh,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(slug, "update")
            .req_context(final_ctx.context)
            .user(user)
            .build(),
        &tx,
    )?;

    // Hydrate join fields + strip read-denied fields
    let mut doc = doc;
    query::hydrate_document(&tx, &gtable, &def.fields, &mut doc, None, None)?;

    let read_denied = wh.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

    tx.commit().context("Commit transaction")?;
    Ok(doc)
}
