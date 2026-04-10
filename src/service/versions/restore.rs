//! Version restore operations for collections and globals.

use anyhow::Context as _;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, collection::GlobalDefinition},
    db::{AccessResult, DbConnection, DbPool, query, query::helpers::global_table},
    hooks::HookRunner,
    service::{RunnerWriteHooks, ServiceError, hooks::WriteHooks},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Restore a collection document to a specific version snapshot.
///
/// Opens a transaction, checks update access, finds the version, applies the snapshot,
/// adjusts ref counts, and creates a new version record.
#[allow(clippy::too_many_arguments)]
pub fn restore_collection_version(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &CollectionDefinition,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
    user: Option<&Document>,
    override_access: bool,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    if override_access {
        wh = wh.with_override_access();
    }

    let doc = restore_collection_version_core(
        &tx,
        &wh,
        slug,
        def,
        document_id,
        version_id,
        locale_config,
        user,
    )?;
    tx.commit().context("Commit")?;
    Ok(doc)
}

/// Core logic for collection version restore on an existing connection/transaction.
/// Caller manages the transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn restore_collection_version_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    def: &CollectionDefinition,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
    user: Option<&Document>,
) -> Result<Document> {
    let access =
        write_hooks.check_access(def.access.update.as_deref(), user, Some(document_id), None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let version = query::find_version_by_id(conn, slug, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let doc = query::restore_version(
        conn,
        slug,
        def,
        document_id,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    Ok(doc)
}

/// Restore a global document to a specific version snapshot.
///
/// Opens a transaction, checks update access, finds the version, applies the snapshot,
/// adjusts ref counts, and creates a new version record.
#[allow(clippy::too_many_arguments)]
pub fn restore_global_version(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &GlobalDefinition,
    version_id: &str,
    locale_config: &LocaleConfig,
    user: Option<&Document>,
    override_access: bool,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);
    if override_access {
        wh = wh.with_override_access();
    }

    let doc = restore_global_version_core(&tx, &wh, slug, def, version_id, locale_config, user)?;
    tx.commit().context("Commit")?;
    Ok(doc)
}

/// Core logic for global version restore on an existing connection/transaction.
/// Caller manages the transaction.
pub(crate) fn restore_global_version_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    def: &GlobalDefinition,
    version_id: &str,
    locale_config: &LocaleConfig,
    user: Option<&Document>,
) -> Result<Document> {
    let access = write_hooks.check_access(def.access.update.as_deref(), user, None, None)?;
    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let gtable = global_table(slug);

    let version = query::find_version_by_id(conn, &gtable, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let doc = query::restore_global_version(
        conn,
        slug,
        def,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    Ok(doc)
}
