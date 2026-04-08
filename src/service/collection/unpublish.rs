//! Collection document unpublish.

use anyhow::Context as _;

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbPool, query},
    hooks::{HookContext, HookEvent, HookRunner},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceError, WriteHooks, run_after_change_hooks,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a versioned document: access check → before-hooks → set draft status →
/// after-hooks → hydrate → strip read-denied fields → commit.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
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
    let access = wh.check_access(def.access.update.as_deref(), user, Some(id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let doc = query::find_by_id_raw(&tx, slug, def, id, None, false)?
        .ok_or_else(|| ServiceError::NotFound(format!("Document '{id}' not found in '{slug}'")))?;

    let hook_ctx = HookContext::builder(slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

    crate::service::persist_unpublish(&tx, slug, id, def)?;

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
    query::hydrate_document(&tx, slug, &def.fields, &mut doc, None, None)?;

    let read_denied = wh.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

    tx.commit().context("Commit transaction")?;
    Ok(doc)
}
