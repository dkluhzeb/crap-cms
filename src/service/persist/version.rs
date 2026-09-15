//! DB write phase for draft version saves and unpublish operations.

use anyhow::{Result, anyhow};

use serde_json::Value;

use crate::{
    core::{Document, DocumentFields, field::FieldDefinition},
    db::{LocaleContext, ops, query},
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
/// stamped `_status = "draft"`.
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

/// Persist an unpublish operation: find existing doc, set status to draft,
/// create a draft version snapshot. Returns the existing doc.
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

    // Same reasoning as `unpublish_document_in_conn`: when the def has localized
    // fields and locales are enabled, the bare-column fallback in
    // `find_by_id_raw` references columns that don't exist (`title` instead
    // of `title__en` / `title__de`). Build a default LocaleContext from the
    // attached locale config so the snapshot read fetches every locale's
    // value (the version snapshot must preserve all locales, not just one).
    let locale_ctx = ctx.default_locale_ctx();

    let doc = query::find_by_id_raw(conn, slug, def, id, locale_ctx.as_ref(), false)?
        .ok_or_else(|| anyhow!("Document {id} not found in {slug}"))?;

    versions::unpublish_with_snapshot(
        conn,
        slug,
        id,
        &def.fields,
        def.versions.as_ref(),
        &doc,
        ctx.locale_config,
    )?;

    Ok(doc)
}
