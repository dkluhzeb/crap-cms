//! DB write phase for collection document creation.

use anyhow::Result;

use crate::{
    core::{Document, DocumentFields, reject_nul_characters},
    db::query,
    service::{
        PersistOptions, ServiceContext, persist::sync_search_index, versions,
        write::refuse_unreadable_references,
    },
};

/// Persist the DB write phase of a create operation.
/// Performs: insert -> join data -> password -> version snapshot.
///
/// # Errors
///
/// Returns a backend error if the INSERT, join-table writes, or version
/// snapshot creation fails.
pub fn persist_create(
    ctx: &ServiceContext,
    data: &DocumentFields,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;
    let slug = ctx.slug;

    let locale_cfg = opts.locale_config.cloned().unwrap_or_default();
    let status = if opts.is_draft { "draft" } else { "published" };

    // Final post-hook data: a NUL a before-change hook (or a write that skips
    // validation) put anywhere in the document is refused here.
    reject_nul_characters(data, &def.fields)?;

    // `doc` here carries FLAT `group__sub` columns: `query::create` re-reads via
    // `find_by_id_raw`, which does NOT hydrate groups (only the read-path
    // `hydrate_document` nests them). The version snapshot's own
    // `build_snapshot` re-hydrate relies on this flat shape; the service layer
    // hydrates `doc` to nested afterwards for hooks/return. (The FTS sync reads
    // the row itself and is independent of this shape.)
    let mut doc = query::create(conn, slug, def, data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, data, opts.locale_ctx)?;

    if let Some(pw) = opts.password
        && !pw.is_empty()
    {
        query::update_password(conn, slug, &doc.id, pw)?;
    }

    if def.has_versions() {
        // Also stamps `doc` with the status the row ends with: `query::create`
        // read the row back while it still carried the column default.
        let id = doc.id.clone();
        let snap_ctx =
            versions::VersionSnapshotCtx::for_collection(slug, &id, def, ctx.locale_config);
        versions::create_version_snapshot(conn, &snap_ctx, status, &mut doc)?;
    }

    sync_search_index(ctx, conn, &doc.id, &locale_cfg)?;

    // Ref counts last: they lock every referenced target row (e.g. the shared
    // author), and those locks are held until COMMIT — through the upload
    // settle, the read-back and the after-change hooks that still run in this
    // transaction. Taking them as late as the persist allows keeps that span
    // short. A refused reference is reported on the field holding it.
    // A new reference to a document the writer may not read is refused
    // exactly like one to a missing document.
    query::ref_count::after_create_from_data(conn, &def.fields, data, &locale_cfg)
        .and_then(|added| refuse_unreadable_references(ctx, &added, opts.locale_ctx))
        .map_err(|e| query::ref_count::anchor_to_fields(e, &def.fields, data))?;

    Ok(doc)
}
