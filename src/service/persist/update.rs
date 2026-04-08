//! DB write phase for collection document update and bulk update.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document},
    db::{DbConnection, LocaleContext, query},
    service::{PersistOptions, versions},
};

/// Persist the DB write phase of a normal (non-draft) update operation.
/// Performs: update -> join data -> password -> version snapshot (published).
pub fn persist_update(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, Value>,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let locale_cfg = opts.locale_config.cloned().unwrap_or_default();
    let old_refs =
        query::ref_count::snapshot_outgoing_refs(conn, slug, id, &def.fields, &locale_cfg)?;

    let doc = query::update(conn, slug, def, id, final_data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, hook_data, opts.locale_ctx)?;

    query::ref_count::after_update(conn, slug, &doc.id, &def.fields, &locale_cfg, old_refs)?;

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
        versions::create_version_snapshot(conn, &ctx, "published", &doc)?;
    }

    if conn.supports_fts() {
        query::fts::fts_upsert(conn, slug, &doc, Some(def))?;
    }
    Ok(doc)
}

/// Persist the DB write phase of a single document in a bulk update.
///
/// Handles: partial update -> join data -> ref count adjustment -> FTS sync -> version snapshot.
/// Used by both gRPC UpdateMany and Lua update_many to avoid duplicating per-doc persistence logic.
#[allow(clippy::too_many_arguments)]
pub fn persist_bulk_update(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, Value>,
    locale_ctx: Option<&LocaleContext>,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let old_refs =
        query::ref_count::snapshot_outgoing_refs(conn, slug, id, &def.fields, locale_config)?;

    let updated = query::update_partial(conn, slug, def, id, final_data, locale_ctx)?;

    query::save_join_table_data(conn, slug, &def.fields, id, hook_data, locale_ctx)?;

    query::ref_count::after_update(conn, slug, id, &def.fields, locale_config, old_refs)?;

    if conn.supports_fts() {
        query::fts::fts_upsert(conn, slug, &updated, Some(def))?;
    }

    if def.has_versions() {
        let vs_ctx = versions::VersionSnapshotCtx::builder(slug, &updated.id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .build();
        versions::create_version_snapshot(conn, &vs_ctx, "published", &updated)?;
    }

    Ok(updated)
}
