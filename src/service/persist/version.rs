//! DB write phase for draft version saves and unpublish operations.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::{
    core::Document,
    db::{LocaleContext, query},
    service::{ServiceContext, versions},
};

/// Persist a draft-only version save: find existing doc, merge incoming data,
/// create a draft version snapshot. Main table is NOT modified.
pub fn persist_draft_version(
    ctx: &ServiceContext,
    id: &str,
    hook_data: &HashMap<String, Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();
    let slug = ctx.slug;

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
pub fn persist_unpublish(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();
    let slug = ctx.slug;

    // Same reasoning as `unpublish_document_core`: when the def has localized
    // fields and locales are enabled, the bare-column fallback in
    // `find_by_id_raw` references columns that don't exist (`title` instead
    // of `title__en` / `title__de`). Build a default LocaleContext from the
    // attached locale config so the snapshot read fetches every locale's
    // value (the version snapshot must preserve all locales, not just one).
    let locale_ctx = ctx.default_locale_ctx();

    let doc = query::find_by_id_raw(conn, slug, def, id, locale_ctx.as_ref(), false)?
        .ok_or_else(|| anyhow!("Document {} not found in {}", id, slug))?;

    versions::unpublish_with_snapshot(conn, slug, id, &def.fields, def.versions.as_ref(), &doc)?;

    Ok(doc)
}
