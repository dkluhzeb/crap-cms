//! Collection document creation.

use anyhow::Context as _;

use crate::{
    core::{CollectionDefinition, Document},
    db::{BoxedConnection, DbPool},
    hooks::HookRunner,
    service::{RunnerWriteHooks, ServiceError, WriteInput, WriteResult, create_document_core},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Create a document within a single transaction: before-hooks -> insert -> after-hooks -> commit.
/// When `draft` is true and the collection has drafts enabled, the document is created with
/// `_status = 'draft'` and required-field validation is skipped.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn create_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
    override_access: bool,
) -> Result<WriteResult> {
    let mut conn = pool.get().context("DB connection")?;
    create_document_with_conn(&mut conn, runner, slug, def, input, user, override_access)
}

/// Like [`create_document`], but accepts an existing connection (avoids a second pool.get()).
pub fn create_document_with_conn(
    conn: &mut BoxedConnection,
    runner: &HookRunner,
    slug: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
    override_access: bool,
) -> Result<WriteResult> {
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    if override_access {
        wh = wh.with_override_access();
    }

    let result = create_document_core(&tx, &wh, slug, def, input, user)?;
    tx.commit().context("Commit transaction")?;
    Ok(result)
}
