//! DB write phase for collection document creation.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{CollectionDefinition, Document},
    db::{DbConnection, query},
    service::{PersistOptions, versions},
};

/// Persist the DB write phase of a create operation.
/// Performs: insert -> join data -> password -> version snapshot.
pub fn persist_create(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, Value>,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let locale_cfg = opts.locale_config.cloned().unwrap_or_default();
    let status = if opts.is_draft { "draft" } else { "published" };
    let doc = query::create(conn, slug, def, final_data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, hook_data, opts.locale_ctx)?;

    query::ref_count::after_create(conn, slug, &doc.id, &def.fields, &locale_cfg)?;

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
    Ok(doc)
}
