//! Collection document deletion.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, upload, upload::StorageBackend},
    db::{BoxedConnection, DbPool},
    hooks::HookRunner,
    service::{RunnerWriteHooks, ServiceError, delete_document_core},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Delete a document within a single transaction: before-hooks -> delete -> after-hooks -> commit.
/// If `config_dir` is provided and the collection is an upload collection,
/// upload files are cleaned up after successful deletion.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub fn delete_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
    override_access: bool,
) -> Result<HashMap<String, Value>> {
    let mut conn = pool.get().context("DB connection")?;
    delete_document_with_conn(
        &mut conn,
        runner,
        slug,
        id,
        def,
        user,
        storage,
        locale_config,
        override_access,
    )
}

/// Like [`delete_document`], but accepts an existing connection.
#[allow(clippy::too_many_arguments)]
pub fn delete_document_with_conn(
    conn: &mut BoxedConnection,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
    override_access: bool,
) -> Result<HashMap<String, Value>> {
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    if override_access {
        wh = wh.with_override_access();
    }

    let result = delete_document_core(&tx, &wh, slug, id, def, user, locale_config)?;
    tx.commit().context("Commit transaction")?;

    // Clean up upload files after successful commit (skip for soft-delete to allow restore)
    if !def.soft_delete
        && let (Some(s), Some(fields)) = (storage, result.upload_doc_fields)
    {
        upload::delete_upload_files(s, &fields);
    }

    Ok(result.context)
}
