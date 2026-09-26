//! Version restore operations for collections and globals.

use std::collections::HashSet;

use serde_json::Value;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{
        Document, DocumentFields, EventViewPlacement, FieldChildren, FieldDefinition,
        canonicalize_text_values, document::VersionSnapshot, event::EventOperation, field_children,
    },
    db::{
        AccessResult, DbConnection, LocaleContext, query,
        query::helpers::{global_table, prefixed_name},
    },
    hooks::{AccessCheckInput, ValidationCtx},
    service::{
        Gated, ServiceContext, ServiceError, SnapshotReadKeep, StoredByLocale,
        global_access_allowed, helpers,
        hooks::{SnapshotLocales, WriteHooks},
        invalidate_user_streams_if_auth,
        persist::sync_search_index,
        run_pool_write, stored_fields_for_update_rules, stored_global_fields_for_update_rules,
        versions::gate::versions_gate_decision,
        write::{
            StoredLoader, UploadSettle, adopt_held_variants, claim_revision, document_file_keys,
            restored_file_conversions, settle_upload_write,
        },
    },
};

/// The status a restore writes, records and validates at.
///
/// A draft snapshot restores as a draft and a published one as published,
/// rather than force-publishing every restore. Without drafts there is no
/// unpublished state: the restored content is live the moment it is written,
/// so it restores as `published` — and is validated at full strictness —
/// whatever the snapshot was stamped. A `draft` version survives on such a
/// definition when drafts were switched off after it was saved; restoring it
/// at draft leniency put a document missing a required value in front of
/// every reader.
fn restore_status(has_drafts: bool, version_status: &str) -> String {
    if has_drafts {
        return version_status.to_string();
    }

    "published".to_string()
}

/// Lock the row a restore writes, then read where it sits across the content
/// views going in. Taken before anything the restore builds on is read — the
/// field-write rules judge the live row, and a restore that changes the
/// status (a draft version over a published row, or a published version over
/// a draft) moves the row between views, which its live event announces — so
/// a concurrent write cannot change the row in between (Postgres; a no-op on
/// `SQLite`, whose transaction already serializes writers). A restore targets a
/// live row, and without drafts there is no status view to read.
///
/// A restore rewrites the document, so it also moves the row's revision
/// forward: an editor who loaded the document before the restore is refused
/// when they save over it.
fn lock_and_read_placement(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    has_drafts: bool,
) -> Result<EventViewPlacement> {
    conn.lock_row(table, id)?;
    claim_revision(conn, table, id, None)?;

    let status = if has_drafts {
        query::get_document_status(conn, table, id)?
    } else {
        None
    };

    Ok(EventViewPlacement {
        status,
        trashed: false,
    })
}

/// Bring a snapshot's email and text values into the canonical form every
/// write stores. A snapshot taken before values were stored that way holds
/// them as typed, and uniqueness validation and the restored row must see the
/// stored form.
fn canonicalize_snapshot(snapshot: &mut Value, fields: &[FieldDefinition]) {
    if let Some(obj) = snapshot.as_object_mut() {
        canonicalize_text_values(obj, fields);
    }
}

/// Take every value the restorer cannot read out of the snapshot, so the
/// restore leaves it at its current value: a write never changes a value its
/// writer cannot read, and a restore is a write. The restore is partial for
/// such a writer, exactly as it is for a field they may not write. Each locale
/// the restore writes is judged against the document as that locale stores it
/// (`load`), a shared value at the restore's own locale.
///
/// # Errors
///
/// Returns the error a stored-document read returns, or an error if a
/// configured locale code has no column form.
fn keep_unreadable_current(
    ctx: &ServiceContext,
    snapshot: &mut Value,
    locale_ctx: Option<&LocaleContext>,
    load: &StoredLoader<'_>,
) -> Result<()> {
    let fields = ctx.fields()?;
    let write_hooks = ctx.write_hooks()?;

    // A system write reads everything, so it keeps nothing.
    if write_hooks.overrides_access() {
        return Ok(());
    }

    let by_locale = StoredByLocale::load(fields, locale_ctx, load)?;

    write_hooks.keep_unreadable_value(
        fields,
        snapshot,
        &SnapshotReadKeep::builder(&by_locale, ctx.slug)
            .user(ctx.user)
            .locale_ctx(locale_ctx)
            .build(),
    )?;

    Ok(())
}

/// Convert a snapshot JSON object into a `DocumentFields` suitable
/// for `validate_fields`. The snapshot's top-level keys are field names
/// (group fields appear in either flat `seo__title` or nested `seo: {…}`
/// form — the validator handles both via the schema walk).
fn snapshot_to_validation_data(snapshot: &Value) -> DocumentFields {
    snapshot
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// Collect every column/field name expected by the current schema for a given
/// field list. Used to detect snapshot keys that have drifted out of the
/// current schema at restore time.
///
/// Includes:
/// - scalar field names,
/// - group-prefixed sub-field names (e.g. `seo__title`),
/// - layout-wrapper children (tabs/rows/collapsibles are transparent),
/// - Blocks/Array/Relationship top-level names (join data),
/// - companion columns (a timezone date's `_tz`, a code field's `_lang`),
/// - system columns (`created_at`, `updated_at`).
fn collect_known_keys(fields: &[FieldDefinition], prefix: &str, out: &mut HashSet<String>) {
    for f in fields {
        match field_children(f) {
            FieldChildren::Group(sub) => {
                let new_prefix = prefixed_name(prefix, &f.name);
                // Nested form is also valid in snapshots.
                out.insert(f.name.clone());
                collect_known_keys(sub, &new_prefix, out);
            }
            FieldChildren::Wrapper(sub) => {
                collect_known_keys(sub, prefix, out);
            }
            FieldChildren::Tabs(tabs) => {
                for t in tabs {
                    collect_known_keys(&t.fields, prefix, out);
                }
            }
            // Array/Blocks (join-backed) and scalar leaves all register their
            // own key: the snapshot extractor accepts both the prefixed and the
            // bare name, plus its companions (`_tz`, `_lang`).
            FieldChildren::Array(_) | FieldChildren::Blocks(_) | FieldChildren::Leaf => {
                let key = prefixed_name(prefix, &f.name);
                out.extend(f.columns_with_companions(&key));
                out.extend(f.columns_with_companions(&f.name));
            }
        }
    }
}

/// Warn about each snapshot key that no longer maps to the current schema.
/// Silent-drop behavior is preserved — this purely adds visibility.
fn warn_on_snapshot_drift(
    snapshot: &Value,
    fields: &[FieldDefinition],
    slug: &str,
    version_id: &str,
) {
    // Accept standard document metadata + locale suffixes transparently.
    const METADATA: &[&str] = &[
        "id",
        "created_at",
        "updated_at",
        "_status",
        "_trashed_at",
        "_ref_count",
    ];

    let Some(obj) = snapshot.as_object() else {
        return;
    };

    let mut known: HashSet<String> = HashSet::new();
    collect_known_keys(fields, "", &mut known);

    for key in obj.keys() {
        if METADATA.contains(&key.as_str()) {
            continue;
        }

        if known.contains(key) {
            continue;
        }

        // Locale-suffixed variant: strip trailing `__xx` and retry.
        if let Some(idx) = key.rfind("__")
            && known.contains(&key[..idx])
        {
            continue;
        }

        warn!(
            "restoring version {} of {}: snapshot key '{}' no longer exists in current schema — ignored",
            version_id, slug, key
        );
    }
}

type Result<T> = std::result::Result<T, ServiceError>;

/// Restore a collection document to a specific version snapshot.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// # Errors
///
/// Returns `AccessDenied`, `NotFound`, or `Validation` errors as appropriate.
/// Returns a backend error if the DB transaction or persistence fails.
pub fn restore_collection_version(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    // Authoritative capability gate: a non-versioned collection has no version
    // table to restore from. Enforced at the one service chokepoint —
    // previously only the gRPC codec checked, so MCP/Lua surfaced a raw
    // missing-table DB error.
    if !ctx.has_versions() {
        return Err(ServiceError::HookError(format!(
            "'{}' does not have versioning enabled",
            ctx.slug
        )));
    }

    if ctx.pool.is_some() {
        restore_collection_version_pool(ctx, document_id, version_id, locale_config)
    } else {
        restore_collection_version_conn(ctx, document_id, version_id, locale_config)
    }
}

fn restore_collection_version_pool(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let (doc, _) = run_pool_write(
        ctx,
        None,
        |inner| restore_collection_version_core(inner, document_id, version_id, locale_config),
        |ctx, (_, row)| {
            ctx.publish_mutation_event(EventOperation::Restore, document_id, row.clone());
            // Restoring an auth document can change that user's access.
            invalidate_user_streams_if_auth(ctx, document_id);
        },
    )?;

    Ok(doc)
}

fn restore_collection_version_conn(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let (doc, row) = restore_collection_version_core(ctx, document_id, version_id, locale_config)?;

    ctx.clear_cache();
    ctx.publish_mutation_event(EventOperation::Restore, document_id, row);
    invalidate_user_streams_if_auth(ctx, document_id);

    Ok(doc)
}

/// Enforce the explicit `access.versions` toggle on restore. Resurrecting a
/// historical snapshot into the live document is a read of version history, so a
/// user with an explicit `access.versions = false` cannot restore even a known
/// `version_id` — the `versions` boundary covers historical *content*, not just
/// its listing.
///
/// When the toggle is **unset**, this is a no-op: restore is already gated by
/// `access.update` against the target document by the caller (see
/// [`restore_collection_version_core`]), which is exactly what the
/// `versions ?? update` fallback resolves to — so there is nothing extra to
/// check here. The explicit toggle therefore only ever *further* restricts.
fn check_restore_versions_gate(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    id: Option<&str>,
) -> Result<()> {
    let Some(versions_ref) = ctx.versions_access_ref() else {
        return Ok(());
    };

    let access = write_hooks.check_access(
        &AccessCheckInput::builder("restore", ctx.slug)
            .access(Some(versions_ref))
            .user(ctx.user)
            .id(id)
            .build(),
    )?;

    versions_gate_decision(&access, ctx.slug)
}

/// Admit a restore of `version_id` onto `document_id`: the caller may update
/// the document (row constraints included) and read its version history, and
/// the version belongs to that document. Returns the version.
fn authorize_restore(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    document_id: &str,
    version_id: &str,
) -> Result<VersionSnapshot> {
    let def = ctx.collection_def()?;

    let access = write_hooks.check_access(
        &AccessCheckInput::builder("restore", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .id(Some(document_id))
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // Row-level enforcement for Constrained: target row must match the filters.
    helpers::enforce_access_constraints(ctx, document_id, &access, "Update", false)?;

    // Restore also requires version-history access (explicit `versions` toggle;
    // an unset toggle is already covered by the `update` check above).
    check_restore_versions_gate(ctx, write_hooks, Some(document_id))?;

    let version = query::find_version_by_id(conn, ctx.slug, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    // The version must belong to the target document. Without this check, a
    // caller with update access to one document could restore ANOTHER
    // document's snapshot onto it (cross-document snapshot injection,
    // bypassing row-level read filters on the source). NotFound rather than
    // AccessDenied so version ids can't be probed across documents.
    if version.parent.as_ref() != document_id {
        return Err(ServiceError::NotFound(format!(
            "Version '{version_id}' not found"
        )));
    }

    Ok(version)
}

/// Refuse a restore onto a trashed or missing document.
fn ensure_live_target(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    document_id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let def = ctx.collection_def()?;

    // A restore must target a LIVE document. Restoring onto a soft-deleted
    // (trashed) row would silently rewrite its fields and record new version
    // history while the row stays invisible in the trash view — an update op
    // must not apply to a trashed target. `find_by_id` excludes trashed rows,
    // so a missing row here means the target is trashed or gone → NotFound
    // (the same fail-closed shape as the cross-document guard in
    // `authorize_restore`).
    if def.soft_delete && query::find_by_id(conn, ctx.slug, def, document_id, locale_ctx)?.is_none()
    {
        return Err(ServiceError::NotFound(format!(
            "Document '{document_id}' not found"
        )));
    }

    Ok(())
}

/// Core logic for collection version restore on an existing connection/transaction.
/// Returns the stored row the restore event is built from alongside the
/// document.
pub(crate) fn restore_collection_version_core(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Gated<Document>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    let version = authorize_restore(ctx, conn, write_hooks, document_id, version_id)?;

    // The default-locale context, reused below for the trashed-target guard and
    // the completeness-aware validation.
    let restore_locale_ctx = LocaleContext::default_for(locale_config);

    ensure_live_target(ctx, conn, document_id, restore_locale_ctx.as_ref())?;

    // Restore returns the document to its exact state at that point in time —
    // including its publication status (see `restore_status`).
    let restored_status = restore_status(def.has_drafts(), &version.status);
    let mut snapshot = version.snapshot;

    warn_on_snapshot_drift(&snapshot, &def.fields, ctx.slug, version_id);

    // Field-level write access also gates restore: a user who may `update` the
    // document but is write-denied on a specific field cannot use a restore to
    // overwrite that field's live value. Drop write-denied fields from the
    // snapshot before validation and persistence (same input-stripping model
    // `update` uses), so the partial restore leaves their stored values intact.
    // Rules judge the live row, not the snapshot being restored.
    let prior = lock_and_read_placement(conn, ctx.slug, document_id, def.has_drafts())?;

    // The row as the restore finds it: a restore that changes the status
    // also replaces the content, and the view the row leaves is judged
    // against what it held there.
    let before = ctx.row_before_write(document_id, Some(locale_config))?;

    let stored = stored_fields_for_update_rules(
        conn,
        ctx.slug,
        def,
        document_id,
        restore_locale_ctx.as_ref(),
    )?;
    write_hooks.strip_write_access_value(
        &def.fields,
        &mut snapshot,
        &stored,
        ctx.slug,
        ctx.user,
        SnapshotLocales::for_write(restore_locale_ctx.as_ref()),
    );

    let load = |locale_ctx: Option<&LocaleContext>| {
        stored_fields_for_update_rules(conn, ctx.slug, def, document_id, locale_ctx)
    };
    keep_unreadable_current(ctx, &mut snapshot, restore_locale_ctx.as_ref(), &load)?;

    canonicalize_snapshot(&mut snapshot, &def.fields);

    // Re-run schema validation against the restored data, so a snapshot
    // saved before a schema tightening (e.g. a field gained `required = true`
    // or a stricter regex) is rejected rather than silently overwriting
    // valid live data with invalid contents. User-defined hooks are not
    // re-run — restore is meant to be transparent — but type / required /
    // unique / regex constraints from the current schema bite.
    let validation_data = snapshot_to_validation_data(&snapshot);

    // Validate at the strictness the write path uses: a snapshot restored as
    // PUBLISHED must satisfy the localized-completeness (`required_locales`)
    // gate, a draft restore is exempt — mirroring create/update. Without the
    // locale context and required-locales the completeness check silently
    // no-ops, so a published snapshot missing a required localized value (a
    // field added or tightened after the snapshot) would restore anyway.
    //
    // Every locale is written from the snapshot below, so the snapshot is also
    // what completeness judges: the live row's translations are the ones this
    // restore replaces, not the ones it leaves behind.
    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .exclude_id(Some(document_id))
        .soft_delete(def.soft_delete)
        .draft(restored_status == "draft")
        .locale_ctx(restore_locale_ctx.as_ref())
        .collection_required_locales(def.required_locales.as_ref())
        .user(ctx.user)
        .locale_overlay(snapshot.as_object())
        .build();
    write_hooks
        .validate_fields(&def.fields, &validation_data, &val_ctx)
        .map_err(ServiceError::Validation)?;

    // Restoring records a version like every other lifecycle step, so it prunes
    // like one — and pruning a snapshot is what drops a stored file's last
    // reference. The files the document referenced going in are read here so
    // the settle below can release exactly the ones nothing names any more.
    let before_files = document_file_keys(ctx, def, document_id, restore_locale_ctx.as_ref())?;

    // The snapshot recorded its queued variants empty: name the ones whose
    // bytes the document still holds, and owe the rest as jobs (below).
    adopt_held_variants(def, &before_files, &mut snapshot);

    let mut doc = query::restore_version(
        conn,
        ctx.slug,
        def,
        document_id,
        &snapshot,
        &restored_status,
        locale_config,
    )?;

    // Re-sync the search index to the restored content.
    sync_search_index(ctx, conn, document_id, locale_config)?;

    // The restored file's queued variants the document no longer holds.
    let conversions =
        restored_file_conversions(def, &before_files, &doc.fields, ctx.image_max_attempts);

    settle_upload_write(
        ctx,
        &UploadSettle::builder(def, document_id)
            .before(Some(&before_files))
            .updated_row(Some(&doc.fields))
            .conversions(conversions.as_ref())
            .build(),
    )?;

    helpers::hydrate_reported(ctx, &mut doc, restore_locale_ctx.as_ref())?;

    // The row as stored, before anything is shaped or stripped for the writer:
    // the live event is built from it. A restore that changes the status moves
    // the row between the published and draft views, which the event
    // announces as a removal to the subscribers that could only see it where
    // it was — judged against the content it had there.
    let row = ctx
        .event_row(&doc)
        .map(|row| row.moved_from(Some(prior)).left_as(before));

    helpers::strip_reported(ctx, write_hooks, &mut doc, restore_locale_ctx.as_ref())?;

    Ok((doc, row))
}

/// Restore a global document to a specific version snapshot.
///
/// # Errors
///
/// Returns `AccessDenied`, `NotFound`, `HookError` (for constrained access on
/// a global, which is not supported), or `Validation` errors as appropriate.
/// Returns a backend error if the DB transaction or persistence fails.
pub fn restore_global_version(
    ctx: &ServiceContext,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    // Capability gate at the one service chokepoint, as for collections.
    if !ctx.has_versions() {
        return Err(ServiceError::HookError(format!(
            "'{}' does not have versioning enabled",
            ctx.slug
        )));
    }

    if ctx.pool.is_some() {
        let (doc, _) = run_pool_write(
            ctx,
            None,
            |inner| restore_global_version_core(inner, version_id, locale_config),
            |ctx, (_, row)| {
                ctx.publish_mutation_event(EventOperation::Restore, "default", row.clone());
            },
        )?;

        return Ok(doc);
    }

    let (doc, row) = restore_global_version_core(ctx, version_id, locale_config)?;

    ctx.clear_cache();
    ctx.publish_mutation_event(EventOperation::Restore, "default", row);

    Ok(doc)
}

/// Core logic for global version restore on an existing connection/transaction.
/// Returns the stored row the restore event is built from alongside the
/// document.
pub(crate) fn restore_global_version_core(
    ctx: &ServiceContext,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Gated<Document>> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def()?;

    let access = write_hooks.check_access(
        &AccessCheckInput::builder("restore", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .id(Some("default"))
            .build(),
    )?;

    if !global_access_allowed(&access, ctx.slug)? {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // Restore also requires version-history access (explicit `versions` toggle;
    // an unset toggle is already covered by the `update` check above).
    check_restore_versions_gate(ctx, write_hooks, None)?;

    let gtable = global_table(ctx.slug);

    let version = query::find_version_by_id(conn, &gtable, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    // Restore to the snapshot's own publication status (see `restore_status`)
    // rather than force-publishing.
    let restored_status = restore_status(def.has_drafts(), &version.status);
    let mut snapshot = version.snapshot;

    warn_on_snapshot_drift(&snapshot, &def.fields, ctx.slug, version_id);

    // Field-level write access also gates restore — see the collection variant
    // above. Drop write-denied fields from the snapshot before validation and
    // persistence so a restore can't overwrite a write-locked field's value.
    // Rules judge the live global, not the snapshot being restored.
    let restore_locale_ctx = LocaleContext::default_for(locale_config);
    let prior = lock_and_read_placement(conn, &gtable, "default", def.has_drafts())?;

    let stored =
        stored_global_fields_for_update_rules(conn, ctx.slug, def, restore_locale_ctx.as_ref())?;
    write_hooks.strip_write_access_value(
        &def.fields,
        &mut snapshot,
        &stored,
        ctx.slug,
        ctx.user,
        SnapshotLocales::for_write(restore_locale_ctx.as_ref()),
    );

    let load = |locale_ctx: Option<&LocaleContext>| {
        stored_global_fields_for_update_rules(conn, ctx.slug, def, locale_ctx)
    };
    keep_unreadable_current(ctx, &mut snapshot, restore_locale_ctx.as_ref(), &load)?;

    canonicalize_snapshot(&mut snapshot, &def.fields);

    // Re-run schema validation against the restored data — see the
    // collection variant above for the full rationale.
    let validation_data = snapshot_to_validation_data(&snapshot);

    // Mirror the global update path's validation strictness: draft-aware and
    // locale-scoped, so a published restore enforces localized completeness and
    // a draft restore is exempt (see the collection variant above) — judged
    // against the snapshot every locale is written from, not the translations
    // the restore replaces.
    let val_ctx = ValidationCtx::builder(conn, &gtable)
        .exclude_id(Some("default"))
        .draft(restored_status == "draft")
        .locale_ctx(restore_locale_ctx.as_ref())
        .user(ctx.user)
        .locale_overlay(snapshot.as_object())
        .build();
    write_hooks
        .validate_fields(&def.fields, &validation_data, &val_ctx)
        .map_err(ServiceError::Validation)?;

    let mut doc = query::restore_global_version(
        conn,
        ctx.slug,
        def,
        &snapshot,
        &restored_status,
        locale_config,
    )?;

    // The global as stored, before anything is shaped or stripped for the
    // writer: the live event is built from it (a draft restored over the
    // published global unpublishes it — see the collection variant).
    let row = ctx.event_row(&doc).map(|row| row.moved_from(Some(prior)));

    helpers::strip_reported(ctx, write_hooks, &mut doc, restore_locale_ctx.as_ref())?;

    Ok((doc, row))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::{
        canonicalize_snapshot, collect_known_keys, restore_collection_version, restore_status,
        warn_on_snapshot_drift,
    };
    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, FieldAdmin, FieldDefinition, FieldType},
        service::{ServiceContext, ServiceError},
    };

    /// Regression: the drift check knew only a timezone date's `_tz` companion,
    /// so restoring a snapshot that holds a code field's language warned that
    /// `snippet_lang` (and each `snippet_lang__xx`) no longer exists. The warning
    /// itself isn't capturable without extra deps, so this pins the known-key
    /// set the check consults — its bare, group-prefixed and per-locale forms.
    #[test]
    fn collect_known_keys_includes_code_language_companion() {
        let code = FieldDefinition::builder("snippet", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .build();
        let fields = vec![
            code.clone(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![code])
                .build(),
        ];

        let mut known = HashSet::new();
        collect_known_keys(&fields, "", &mut known);

        assert!(known.contains("snippet_lang"), "{known:?}");
        assert!(known.contains("meta__snippet_lang"), "{known:?}");

        let per_locale = "snippet_lang__de";
        let base = &per_locale[..per_locale.rfind("__").unwrap()];
        assert!(known.contains(base), "per-locale key resolves: {known:?}");
    }

    /// Regression: a restore validated and wrote a snapshot's email and text
    /// values as typed, so a snapshot taken before values were stored
    /// canonically slipped past uniqueness and stored a form no lookup matches.
    #[test]
    fn a_restored_snapshot_is_brought_into_canonical_form() {
        let fields = vec![
            FieldDefinition::builder("email", FieldType::Email).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];
        let mut snapshot = json!({
            "email": " Bob@Example.com",
            "seo": { "title": "Cafe\u{301}" },
        });

        canonicalize_snapshot(&mut snapshot, &fields);

        assert_eq!(
            snapshot,
            json!({ "email": "bob@example.com", "seo": { "title": "Caf\u{e9}" } })
        );
    }

    /// Regression: the `has_versions` gate lives in the service chokepoint —
    /// previously only the gRPC codec checked, so MCP/Lua hit the missing
    /// version table and surfaced a raw DB error instead of a typed one. The
    /// gate fires before mode dispatch, so no connection is needed.
    #[test]
    fn restore_rejects_non_versioned_collection() {
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def).build();

        let err =
            restore_collection_version(&ctx, "p1", "v1", &LocaleConfig::default()).unwrap_err();

        assert!(
            matches!(&err, ServiceError::HookError(msg) if msg.contains("versioning")),
            "expected typed versioning gate error, got {err:?}"
        );
    }

    /// With drafts a restore keeps the snapshot's status; without them every
    /// restore is a publish, validated at full strictness.
    #[test]
    fn restore_status_is_published_without_drafts() {
        assert_eq!(restore_status(true, "draft"), "draft");
        assert_eq!(restore_status(true, "published"), "published");
        assert_eq!(restore_status(false, "draft"), "published");
        assert_eq!(restore_status(false, "published"), "published");
    }

    #[test]
    fn collect_known_keys_scalar_fields() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("body", FieldType::Textarea).build(),
        ];
        let mut known = HashSet::new();
        collect_known_keys(&fields, "", &mut known);
        assert!(known.contains("title"));
        assert!(known.contains("body"));
    }

    #[test]
    fn collect_known_keys_group_fields() {
        let sub = FieldDefinition::builder("title", FieldType::Text).build();
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![sub])
            .build();
        let mut known = HashSet::new();
        collect_known_keys(&[group], "", &mut known);
        assert!(known.contains("seo"));
        assert!(known.contains("seo__title"));
        assert!(known.contains("title")); // bare subfield name is also accepted
    }

    /// Regression: when a snapshot contains keys that no longer exist in the
    /// current schema, `warn_on_snapshot_drift` must emit a `warn!` for each.
    /// We can't capture tracing output without extra deps, so at minimum assert
    /// that (1) the drift helper does not panic for the drift scenario and
    /// (2) `collect_known_keys` does not accept the stale key — the warn path
    /// is therefore exercised.
    #[test]
    fn restore_version_warns_on_unknown_snapshot_key() {
        let fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        let snapshot = json!({
            "id": "doc1",
            "title": "current",
            "old_deprecated_field": "leftover",
            "created_at": "2024-01-01T00:00:00.000Z",
        });

        let mut known = HashSet::new();
        collect_known_keys(&fields, "", &mut known);
        assert!(known.contains("title"));
        assert!(!known.contains("old_deprecated_field"));

        warn_on_snapshot_drift(&snapshot, &fields, "posts", "ver_123");
    }

    #[test]
    fn drift_accepts_metadata_keys() {
        let fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        let snapshot = json!({
            "id": "doc1",
            "title": "t",
            "created_at": "2024",
            "updated_at": "2024",
            "_status": "published",
            "_trashed_at": null,
            "_ref_count": 0,
        });
        warn_on_snapshot_drift(&snapshot, &fields, "posts", "v1");
    }

    #[test]
    fn drift_accepts_locale_suffixed_keys() {
        let fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        let snapshot = json!({
            "title": "t",
            "title__de": "deutsch",
            "title__en": "english",
        });
        warn_on_snapshot_drift(&snapshot, &fields, "posts", "v1");
    }
}
