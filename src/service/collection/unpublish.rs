//! Collection document unpublish.

use anyhow::Context as _;

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, DbPool, query},
    hooks::{HookContext, HookEvent, HookRunner},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceError, WriteHooks, persist_unpublish,
        run_after_change_hooks,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Unpublish a versioned document on an existing connection/transaction.
///
/// Runs the full lifecycle: access check → before-hooks → set draft status →
/// after-hooks → hydrate → strip read-denied fields.
/// Does NOT manage transactions — caller must open/commit.
pub fn unpublish_document_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    // Access check — unpublish requires update access
    let access = write_hooks.check_access(def.access.update.as_deref(), user, Some(id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let doc = query::find_by_id_raw(conn, slug, def, id, None, false)?
        .ok_or_else(|| ServiceError::NotFound(format!("Document '{id}' not found in '{slug}'")))?;

    let hook_ctx = HookContext::builder(slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(user)
        .build();
    let final_ctx =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, conn)?;

    persist_unpublish(conn, slug, id, def)?;

    run_after_change_hooks(
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

    // Hydrate join fields + strip read-denied fields
    let mut doc = doc;
    query::hydrate_document(conn, slug, &def.fields, &mut doc, None, None)?;

    let read_denied = write_hooks.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

    Ok(doc)
}

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
    override_access: bool,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    if override_access {
        wh = wh.with_override_access();
    }

    let doc = unpublish_document_core(&tx, &wh, slug, id, def, user)?;

    tx.commit().context("Commit transaction")?;
    Ok(doc)
}
