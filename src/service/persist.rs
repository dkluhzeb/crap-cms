//! DB write phase functions for collection CRUD operations.
//!
//! Each `persist_*` function handles the database-level work for a single operation:
//! insert/update rows, join table data, passwords, and version snapshots.

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use serde_json::Value;

use crate::{
    core::{CollectionDefinition, Document},
    db::{DbConnection, LocaleContext, query},
};

use super::{PersistOptions, versions};

/// Persist the DB write phase of a create operation.
/// Performs: insert → join data → password → version snapshot.
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

/// Persist the DB write phase of a normal (non-draft) update operation.
/// Performs: update → join data → password → version snapshot (published).
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

/// Persist a draft-only version save: find existing doc, merge incoming data,
/// create a draft version snapshot. Main table is NOT modified.
pub fn persist_draft_version(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    hook_data: &HashMap<String, Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let existing_doc = query::find_by_id_raw(conn, slug, def, id, locale_ctx, false)?
        .ok_or_else(|| anyhow!("Document {} not found in {}", id, slug))?;

    versions::save_draft_version(
        conn,
        slug,
        id,
        &def.fields,
        def.versions.as_ref(),
        &existing_doc,
        hook_data,
    )?;

    Ok(existing_doc)
}

/// Persist an unpublish operation: find existing doc, set status to draft,
/// create a draft version snapshot. Returns the existing doc.
pub fn persist_unpublish(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
) -> Result<Document> {
    let doc = query::find_by_id_raw(conn, slug, def, id, None, false)?
        .ok_or_else(|| anyhow!("Document {} not found in {}", id, slug))?;

    versions::unpublish_with_snapshot(conn, slug, id, &def.fields, def.versions.as_ref(), &doc)?;

    Ok(doc)
}
