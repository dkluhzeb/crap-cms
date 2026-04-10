//! Collection document undelete from soft-delete.

use anyhow::Context as _;

use crate::{
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, DbPool, query},
    hooks::HookRunner,
    service::{RunnerWriteHooks, ServiceError, WriteHooks},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Core undelete logic on an existing connection: access check + restore row + FTS re-sync.
///
/// Checks trash access via `write_hooks.check_access`, then undeletes the document.
/// Does NOT manage transactions -- caller must open/commit.
/// Returns the undeleted document on success.
pub fn undelete_document_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let access = write_hooks.check_access(def.access.resolve_trash(), user, Some(id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Undelete access denied".into()));
    }

    let restored = query::restore(conn, slug, id)?;
    if !restored {
        return Err(ServiceError::NotFound(
            "Document not found or not deleted".into(),
        ));
    }

    // Re-sync FTS index (the FTS row was deleted on soft-delete)
    if conn.supports_fts()
        && let Ok(Some(doc)) = query::find_by_id_unfiltered(conn, slug, def, id, None)
    {
        query::fts::fts_upsert(conn, slug, &doc, Some(def))?;
    }

    query::find_by_id(conn, slug, def, id, None)?
        .ok_or_else(|| ServiceError::NotFound("Document not found after undelete".into()))
}

/// Undelete a soft-deleted document: clear `_deleted_at`, re-sync FTS index.
// Excluded from coverage: requires DB pool + FTS for full integration testing.
// Tested indirectly through admin handler and Lua API tests.
#[cfg(not(tarpaulin_include))]
pub fn undelete_document(
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

    let doc = undelete_document_core(&tx, &wh, slug, id, def, user)?;

    tx.commit()?;
    Ok(doc)
}
