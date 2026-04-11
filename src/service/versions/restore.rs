//! Version restore operations for collections and globals.

use anyhow::Context as _;

use crate::{
    config::LocaleConfig,
    core::Document,
    db::{AccessResult, query, query::helpers::global_table},
    service::{RunnerWriteHooks, ServiceContext, ServiceError, helpers},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Restore a collection document to a specific version snapshot.
pub fn restore_collection_version(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def();
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .build();

    let doc = restore_collection_version_core(&inner_ctx, document_id, version_id, locale_config)?;
    tx.commit().context("Commit")?;
    Ok(doc)
}

/// Core logic for collection version restore on an existing connection/transaction.
pub(crate) fn restore_collection_version_core(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def();

    let access = write_hooks.check_access(
        def.access.update.as_deref(),
        ctx.user,
        Some(document_id),
        None,
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let version = query::find_version_by_id(conn, ctx.slug, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let mut doc = query::restore_version(
        conn,
        ctx.slug,
        def,
        document_id,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(helpers::collect_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok(doc)
}

/// Restore a global document to a specific version snapshot.
pub fn restore_global_version(
    ctx: &ServiceContext,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.global_def();
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::global(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .build();

    let doc = restore_global_version_core(&inner_ctx, version_id, locale_config)?;

    tx.commit().context("Commit")?;

    Ok(doc)
}

/// Core logic for global version restore on an existing connection/transaction.
pub(crate) fn restore_global_version_core(
    ctx: &ServiceContext,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def();

    let access = write_hooks.check_access(def.access.update.as_deref(), ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let gtable = global_table(ctx.slug);

    let version = query::find_version_by_id(conn, &gtable, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let mut doc = query::restore_global_version(
        conn,
        ctx.slug,
        def,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(helpers::collect_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok(doc)
}
