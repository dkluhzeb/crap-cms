//! Collection document update.

use anyhow::Context as _;

use crate::{
    core::{CollectionDefinition, Document},
    db::{BoxedConnection, DbPool},
    hooks::HookRunner,
    service::{RunnerWriteHooks, ServiceError, WriteInput, WriteResult, update_document_core},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Update a document within a single transaction: before-hooks -> update -> after-hooks -> commit.
/// When `draft` is true and the collection has drafts enabled, the update creates a version-only
/// save: the main table is NOT modified, only a new version snapshot is recorded.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn update_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let mut conn = pool.get().context("DB connection")?;
    update_document_with_conn(&mut conn, runner, slug, id, def, input, user)
}

/// Like [`update_document`], but accepts an existing connection.
pub fn update_document_with_conn(
    conn: &mut BoxedConnection,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let tx = conn.transaction_immediate().context("Start transaction")?;
    let wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    let result = update_document_core(&tx, &wh, slug, id, def, input, user)?;
    tx.commit().context("Commit transaction")?;
    Ok(result)
}
