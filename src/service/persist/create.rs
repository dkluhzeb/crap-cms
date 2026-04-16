//! DB write phase for collection document creation.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::Document,
    db::query,
    service::{PersistOptions, ServiceContext, versions},
};

/// Persist the DB write phase of a create operation.
/// Performs: insert -> join data -> password -> version snapshot.
pub fn persist_create(
    ctx: &ServiceContext,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, Value>,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();
    let slug = ctx.slug;

    let locale_cfg = opts.locale_config.cloned().unwrap_or_default();
    let status = if opts.is_draft { "draft" } else { "published" };

    // Lock referenced target rows before INSERT to prevent concurrent deletes
    // from creating dangling references (Postgres only; SQLite serializes via IMMEDIATE).
    query::ref_count::lock_ref_targets_from_data(
        conn,
        &def.fields,
        final_data,
        hook_data,
        &locale_cfg,
    )?;

    let doc = query::create(conn, slug, def, final_data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, hook_data, opts.locale_ctx)?;

    if let Some(pw) = opts.password
        && !pw.is_empty()
    {
        query::update_password(conn, slug, &doc.id, pw)?;
    }

    if def.has_versions() {
        let ctx = versions::VersionSnapshotCtx::builder(slug, &doc.id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .build();
        versions::create_version_snapshot(conn, &ctx, status, &doc)?;
    }

    if conn.supports_fts() {
        query::fts::fts_upsert(conn, slug, &doc, Some(def))?;
    }

    // Ref count UPDATE is last: it acquires a row-level lock on the target
    // (e.g. the referenced author), and that lock is held until COMMIT.
    // Doing it last minimizes lock hold time under concurrent writes.
    query::ref_count::after_create_from_data(
        conn,
        &def.fields,
        final_data,
        hook_data,
        &locale_cfg,
    )?;

    Ok(doc)
}
