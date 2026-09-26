//! DB write phase for draft version saves and unpublish operations.

use anyhow::{Result, anyhow};

use serde_json::Value;

use crate::{
    core::{Document, DocumentFields, field::FieldDefinition, reject_nul_characters},
    db::{LocaleContext, ops, query, query::REVISION_COLUMN},
    service::{ServiceContext, versions},
};

/// Persist a draft-only version save: find existing doc, merge incoming data,
/// create a draft version snapshot. Main table is NOT modified.
///
/// # Errors
///
/// Returns a backend error if the document can't be found or the version
/// snapshot can't be created.
pub fn persist_draft_version(
    ctx: &ServiceContext,
    id: &str,
    hook_data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;
    let slug = ctx.slug;

    // Final post-hook data: the draft snapshot is JSON a `::jsonb` cast reads.
    reject_nul_characters(hook_data, &def.fields)?;

    let existing_doc = query::find_by_id_raw(conn, slug, def, id, locale_ctx, false)?
        .ok_or_else(|| anyhow!("Document {id} not found in {slug}"))?;

    let snapshot = versions::save_draft_version(&versions::SaveDraftArgs {
        conn,
        table: slug,
        parent_id: id,
        fields: &def.fields,
        versions: def.versions.as_ref(),
        existing_doc: &existing_doc,
        data: hook_data,
        locale_ctx,
    })?;

    draft_document(&DraftDocumentArgs {
        id,
        snapshot: &snapshot,
        existing: &existing_doc,
        fields: &def.fields,
        locale_ctx,
    })
}

/// The document a draft save reports: the stored snapshot (the draft content),
/// stamped `_status = "draft"` and the row's `_revision`.
///
/// Shared with the globals draft path so both report the same shape.
///
/// The published row is untouched by a draft save, so returning it — as this
/// path used to — meant `after_change` hooks, the operation's return value and
/// the emitted event all carried the PRE-EDIT document: a published-only
/// subscriber got an Update event for content that had not changed, while a
/// draft subscriber got nothing.
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
pub fn draft_document(args: &DraftDocumentArgs<'_>) -> Result<Document> {
    let &DraftDocumentArgs {
        id,
        snapshot,
        existing,
        fields,
        locale_ctx,
    } = args;

    // A snapshot holds every locale's decorated column. It is read for the
    // locale the write was made under, so the response has the same shape as
    // any other write's — and doesn't hand back translations the caller never
    // asked for.
    let mut doc = ops::snapshot_read_document(id, snapshot, fields, locale_ctx)?
        .unwrap_or_else(|| existing.clone());

    // `draft`, not the row's status: a live event decides who may see a change
    // from `_status`, and a draft save's content must reach only those who may
    // see drafts — even on a published document.
    doc.fields
        .insert("_status".to_string(), Value::String("draft".to_string()));

    // The revision is the row's, never the snapshot's: the draft save moved it
    // forward, and a caller chaining its next write on this response must send
    // the value it has now.
    if let Some(revision) = existing.fields.get(REVISION_COLUMN) {
        doc.fields
            .insert(REVISION_COLUMN.to_string(), revision.clone());
    }

    Ok(doc)
}

/// Inputs for [`draft_document`]. Five fields, built at two call sites and
/// consumed immediately, so a plain struct literal is enough.
pub struct DraftDocumentArgs<'a> {
    pub id: &'a str,
    pub snapshot: &'a Value,
    /// Fallback when the snapshot is not a JSON object.
    pub existing: &'a Document,
    pub fields: &'a [FieldDefinition],
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// Persist an unpublish operation: lock and read the row, set its status to
/// draft, record a draft version snapshot. Returns the stored row, stamped
/// `_status = "draft"`.
///
/// # Errors
///
/// Returns a backend error if the document can't be found, the snapshot
/// can't be created, or the status update fails.
pub fn persist_unpublish(ctx: &ServiceContext, id: &str) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;
    let slug = ctx.slug;

    // The row this snapshots must be the row the status write lands on: on
    // Postgres a concurrent publish committing between an unlocked read and
    // the status UPDATE would otherwise leave a stale draft snapshot as the
    // pending draft. No-op on SQLite.
    conn.lock_row(slug, id)?;

    // Same reasoning as `unpublish_document_in_conn`: when the def has localized
    // fields and locales are enabled, the bare-column fallback in
    // `find_by_id_raw` references columns that don't exist (`title` instead
    // of `title__en` / `title__de`). Build a default LocaleContext from the
    // attached locale config so the snapshot read fetches every locale's
    // value (the version snapshot must preserve all locales, not just one).
    let locale_ctx = ctx.default_locale_ctx();

    let mut doc = query::find_by_id_raw(conn, slug, def, id, locale_ctx.as_ref(), false)?
        .ok_or_else(|| anyhow!("Document {id} not found in {slug}"))?;

    let snap_ctx = versions::VersionSnapshotCtx::for_collection(slug, id, def, ctx.locale_config);
    versions::unpublish_with_snapshot(conn, &snap_ctx, &mut doc)?;

    Ok(doc)
}
