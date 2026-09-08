//! DB write phase for collection document update and bulk update.

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, DocumentFields, collection::Auth},
    db::{DbConnection, LocaleContext, query},
    service::{PersistOptions, ServiceContext, versions, write::reject_locale_locked_fields},
};

/// The address an update is about to write, when it differs from the stored
/// one on a collection that requires email verification.
///
/// Changing the address invalidates the confirmation the old one carried: the
/// new address has never been confirmed, so leaving `_verified` set would let
/// a user log in with an address nobody proved they control.
fn changed_email(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    slug: &str,
    id: &str,
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Option<String> {
    if !def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
        return None;
    }

    let new_email = data.get("email").and_then(|v| v.as_str())?;
    let current = query::find_by_id_raw(conn, slug, def, id, locale_ctx, false)
        .ok()
        .flatten()?;

    (current.get_str("email") != Some(new_email)).then(|| new_email.to_string())
}

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

    let locale_cfg = opts.locale_config.cloned().unwrap_or_default();
    let touches_refs = query::ref_count::data_touches_refs(&def.fields, data, "");

    // Only snapshot + adjust ref counts when the write data actually changes
    // relationship fields. Skipping saves ~10 queries for non-ref updates.
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

    // Detected BEFORE the UPDATE — afterwards the stored address is the new one.
    let new_email = changed_email(conn, def, slug, id, data, opts.locale_ctx);

    let doc = query::update(conn, slug, def, id, data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, data, opts.locale_ctx)?;

    if let Some(pw) = opts.password
        && !pw.is_empty()
    {
        query::update_password(conn, slug, &doc.id, pw)?;
    }

    if new_email.is_some() {
        query::mark_unverified(conn, slug, &doc.id)?;
        // Queued like a create's: the mail goes out only if the write commits.
        ctx.maybe_send_verification(&doc);
    }

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
pub(crate) fn persist_bulk_update(
    ctx: &ServiceContext,
    id: &str,
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    reject_locale_locked_fields(&def.fields, data, locale_ctx)?;

    let touches_refs = query::ref_count::data_touches_refs(&def.fields, data, "");

    let old_refs = if touches_refs {
        // Same row lock as the single-document path: the outgoing-ref snapshot
        // below is unlocked, and two concurrent updates of one row on Postgres
        // would otherwise both read the stale `old_refs` and double-apply.
        conn.lock_row(ctx.slug, id)?;

        query::ref_count::lock_ref_targets_from_data(conn, &def.fields, data, locale_config)?;

        Some(query::ref_count::snapshot_outgoing_refs(
            conn,
            ctx.slug,
            id,
            &def.fields,
            locale_config,
        )?)
    } else {
        None
    };

    let updated = query::update_partial(conn, ctx.slug, def, id, data, locale_ctx)?;

    query::save_join_table_data(conn, ctx.slug, &def.fields, id, data, locale_ctx)?;

    if def.has_versions() {
        let vs_ctx = versions::VersionSnapshotCtx::builder(ctx.slug, &updated.id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .build();
        versions::create_version_snapshot(conn, &vs_ctx, "published", &updated)?;
    }

    if conn.supports_fts() {
        query::fts::fts_upsert(conn, ctx.slug, id, def, locale_config)?;
    }

    // Ref count last: minimizes row-level lock hold time on shared targets.
    if let Some(old_refs) = old_refs {
        query::ref_count::after_update(conn, ctx.slug, id, &def.fields, locale_config, &old_refs)?;
    }

    Ok(updated)
}
