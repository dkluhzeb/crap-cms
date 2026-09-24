//! DB write phase for collection document update and bulk update.

use anyhow::Result;

use crate::{
    core::{Document, DocumentFields, reject_nul_characters},
    db::query,
    service::{
        PersistOptions, ServiceContext,
        persist::email_change::{apply_email_change, email_changed},
        versions,
        write::reject_locale_locked_fields,
    },
};

/// Persist the DB write phase of a normal (non-draft) update operation.
/// Performs: update -> join data -> password -> version snapshot (published).
///
/// # Errors
///
/// Returns a backend error if the UPDATE, join-table writes, or version
/// snapshot creation fails.
pub fn persist_update(
    ctx: &ServiceContext,
    id: &str,
    data: &DocumentFields,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;
    let slug = ctx.slug;

    // Final post-hook data: a before-hook that injected a locale-locked field
    // is rejected here rather than silently skipped at the DB edge.
    reject_locale_locked_fields(&def.fields, data, opts.locale_ctx)?;
    reject_nul_characters(data, &def.fields)?;

    let locale_cfg = opts.locale_config.cloned().unwrap_or_default();

    // A publish makes the WHOLE pending draft live. `data` already carries the
    // draft's values for the locale this write targets; what only a per-locale
    // write-back can put on the row is the draft's OTHER translations and the
    // shared values a default-locale draft save recorded. Without localization
    // there are no other locales and no shared/localized split, so the merged
    // data IS the whole draft and the write-back is skipped.
    let publish_draft = opts.pending_draft.filter(|_| locale_cfg.is_enabled());

    // Only snapshot + adjust ref counts when the write data actually changes
    // relationship fields. Skipping saves ~10 queries for non-ref updates. A
    // draft write-back always counts: it moves relationships of its own, and
    // must land inside the same bracket or its delta is never applied.
    let touches_refs =
        publish_draft.is_some() || query::ref_count::data_touches_refs(&def.fields, data, "");

    let old_refs = if touches_refs {
        // Lock the document row BEFORE the (unlocked) outgoing-ref snapshot so a
        // concurrent update to the same document can't read a stale `old_refs`
        // and double-apply a ref-count delta (Postgres MVCC lets both updates
        // snapshot before either commits → target under/over-counted →
        // delete-protection bypass or a phantom ref). No-op on SQLite, whose
        // IMMEDIATE transaction already serializes writers.
        conn.lock_row(slug, id)?;

        query::ref_count::lock_ref_targets_from_data(conn, &def.fields, data, &locale_cfg)?;

        Some(query::ref_count::snapshot_outgoing_refs(
            conn,
            slug,
            id,
            &def.fields,
            &locale_cfg,
        )?)
    } else {
        None
    };

    // Detected BEFORE anything writes the row — afterwards the stored address
    // is the new one, including when the draft write-back below carries it.
    let address_changed = email_changed(conn, def, slug, id, data, opts.locale_ctx);

    if let Some(pending) = publish_draft {
        query::write_snapshot_base(conn, slug, def, id, pending, &locale_cfg)?;
    }

    let doc = query::update(conn, slug, def, id, data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, data, opts.locale_ctx)?;

    if let Some(pw) = opts.password
        && !pw.is_empty()
    {
        query::update_password(conn, slug, &doc.id, pw)?;
    }

    apply_email_change(ctx, &doc, address_changed)?;

    if def.has_versions() {
        let ctx = versions::VersionSnapshotCtx::builder(slug, &doc.id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .locale_config(ctx.locale_config)
            .build();
        versions::create_version_snapshot(conn, &ctx, "published", &doc)?;
    }

    if conn.supports_fts() {
        query::fts::fts_upsert(conn, slug, &doc.id, def, &locale_cfg)?;
    }

    // Ref count last: minimizes row-level lock hold time on shared targets.
    if let Some(old_refs) = old_refs {
        query::ref_count::after_update(conn, slug, &doc.id, &def.fields, &locale_cfg, &old_refs)?;
    }

    Ok(doc)
}

/// Persist the DB write phase of a single document in a bulk update.
///
/// Handles: partial update -> join data -> ref count adjustment -> FTS sync -> version snapshot.
/// Used by both gRPC `UpdateMany` and Lua `update_many` to avoid duplicating per-doc persistence logic.
///
/// Takes the same [`PersistOptions`] as the single-document path, so a
/// publishing bulk write makes the pending draft live the same way — including
/// the locales the request does not target.
///
/// # Errors
///
/// Returns a backend error if the UPDATE, join-table writes, or version
/// snapshot creation fails.
pub(crate) fn persist_bulk_update(
    ctx: &ServiceContext,
    id: &str,
    data: &DocumentFields,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    reject_locale_locked_fields(&def.fields, data, opts.locale_ctx)?;
    reject_nul_characters(data, &def.fields)?;

    let locale_cfg = opts.locale_config.cloned().unwrap_or_default();

    // See `persist_update`: a publish makes the whole pending draft live, and
    // the write-back that carries its other locales has to sit inside the
    // ref-count bracket below.
    let publish_draft = opts.pending_draft.filter(|_| locale_cfg.is_enabled());

    let touches_refs =
        publish_draft.is_some() || query::ref_count::data_touches_refs(&def.fields, data, "");

    let old_refs = if touches_refs {
        // Same row lock as the single-document path: the outgoing-ref snapshot
        // below is unlocked, and two concurrent updates of one row on Postgres
        // would otherwise both read the stale `old_refs` and double-apply.
        conn.lock_row(ctx.slug, id)?;

        query::ref_count::lock_ref_targets_from_data(conn, &def.fields, data, &locale_cfg)?;

        Some(query::ref_count::snapshot_outgoing_refs(
            conn,
            ctx.slug,
            id,
            &def.fields,
            &locale_cfg,
        )?)
    } else {
        None
    };

    // Same bracket as the single-document path: detected before the row moves
    // to the new address, applied once the write is on disk.
    let address_changed = email_changed(conn, def, ctx.slug, id, data, opts.locale_ctx);

    if let Some(pending) = publish_draft {
        query::write_snapshot_base(conn, ctx.slug, def, id, pending, &locale_cfg)?;
    }

    let updated = query::update_partial(conn, ctx.slug, def, id, data, opts.locale_ctx)?;

    query::save_join_table_data(conn, ctx.slug, &def.fields, id, data, opts.locale_ctx)?;

    apply_email_change(ctx, &updated, address_changed)?;

    if def.has_versions() {
        // The locale config is what makes the snapshot record EVERY locale's
        // column. Without it a bulk-update snapshot held one value per
        // localized field, and restoring it NULLed every other translation —
        // the single-document path above already passes it.
        let vs_ctx = versions::VersionSnapshotCtx::builder(ctx.slug, &updated.id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .locale_config(Some(&locale_cfg))
            .build();
        versions::create_version_snapshot(conn, &vs_ctx, "published", &updated)?;
    }

    if conn.supports_fts() {
        query::fts::fts_upsert(conn, ctx.slug, id, def, &locale_cfg)?;
    }

    // Ref count last: minimizes row-level lock hold time on shared targets.
    if let Some(old_refs) = old_refs {
        query::ref_count::after_update(conn, ctx.slug, id, &def.fields, &locale_cfg, &old_refs)?;
    }

    Ok(updated)
}
