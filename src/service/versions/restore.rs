//! Version restore operations for collections and globals.

use std::collections::HashSet;

use anyhow::Context as _;
use serde_json::Value;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{Document, DocumentFields, FieldDefinition, FieldType, event::EventOperation},
    db::{
        AccessResult, query,
        query::helpers::{global_table, prefixed_name, tz_column},
    },
    hooks::{AccessCheckInput, LuaCrudInfra, ValidationCtx},
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, helpers, hooks::WriteHooks,
        invalidate_user_streams_if_auth, versions::gate::versions_gate_decision,
    },
};

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
/// - optional `_tz` companions for date-with-timezone fields,
/// - system columns (`created_at`, `updated_at`).
fn collect_known_keys(fields: &[FieldDefinition], prefix: &str, out: &mut HashSet<String>) {
    for f in fields {
        match f.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &f.name);
                // Nested form is also valid in snapshots.
                out.insert(f.name.clone());
                collect_known_keys(&f.fields, &new_prefix, out);
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_known_keys(&f.fields, prefix, out);
            }
            FieldType::Tabs => {
                for t in &f.tabs {
                    collect_known_keys(&t.fields, prefix, out);
                }
            }
            _ => {
                let key = prefixed_name(prefix, &f.name);
                out.insert(key.clone());
                // Bare name also accepted by the snapshot extractor.
                out.insert(f.name.clone());

                if f.field_type == FieldType::Date && f.timezone {
                    out.insert(tz_column(&key));
                    out.insert(tz_column(&f.name));
                }
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
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let infra = LuaCrudInfra::from_ctx(ctx, None, None);

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .inherit_write_infra(ctx)
        .build();

    let doc = restore_collection_version_core(&inner_ctx, document_id, version_id, locale_config)?;

    tx.commit().context("Commit")?;

    ctx.clear_cache();
    ctx.publish_mutation_event(EventOperation::Update, document_id, &doc.fields);
    invalidate_user_streams_if_auth(ctx, document_id);

    Ok(doc)
}

fn restore_collection_version_conn(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let doc = restore_collection_version_core(ctx, document_id, version_id, locale_config)?;

    ctx.clear_cache();
    ctx.publish_mutation_event(EventOperation::Update, document_id, &doc.fields);
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

/// Core logic for collection version restore on an existing connection/transaction.
pub(crate) fn restore_collection_version_core(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
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

    // Restore returns the document to its exact state at that point in time —
    // including its publication status. A draft snapshot restores as a draft,
    // a published one as published (rather than force-publishing every
    // restore). For a collection without a status axis the snapshot status is
    // always "published", so this is a no-op there.
    let restored_status = version.status.clone();
    let mut snapshot = version.snapshot;

    warn_on_snapshot_drift(&snapshot, &def.fields, ctx.slug, version_id);

    // Field-level write access also gates restore: a user who may `update` the
    // document but is write-denied on a specific field cannot use a restore to
    // overwrite that field's live value. Drop write-denied fields from the
    // snapshot before validation and persistence (same input-stripping model
    // `update` uses), so the partial restore leaves their stored values intact.
    write_hooks.strip_write_access_value(&def.fields, &mut snapshot, ctx.slug, ctx.user, None);

    // Re-run schema validation against the restored data, so a snapshot
    // saved before a schema tightening (e.g. a field gained `required = true`
    // or a stricter regex) is rejected rather than silently overwriting
    // valid live data with invalid contents. User-defined hooks are not
    // re-run — restore is meant to be transparent — but type / required /
    // unique / regex constraints from the current schema bite.
    let validation_data = snapshot_to_validation_data(&snapshot);
    let val_ctx = ValidationCtx::builder(conn, ctx.slug)
        .exclude_id(Some(document_id))
        .soft_delete(def.soft_delete)
        .user(ctx.user)
        .build();
    write_hooks
        .validate_fields(&def.fields, &validation_data, &val_ctx)
        .map_err(ServiceError::Validation)?;

    let mut doc = query::restore_version(
        conn,
        ctx.slug,
        def,
        document_id,
        &snapshot,
        &restored_status,
        locale_config,
    )?;

    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, None);
    doc.strip_fields(&helpers::collect_api_hidden_field_names(&def.fields, ""));

    Ok(doc)
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
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.global_def()?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let infra = LuaCrudInfra::from_ctx(ctx, None, None);

    let mut wh = RunnerWriteHooks::new(runner)
        .with_conn(&tx)
        .with_infra(infra);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::global(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .cache(ctx.cache.clone())
        .event_transport(ctx.event_transport.clone())
        .build();

    let doc = restore_global_version_core(&inner_ctx, version_id, locale_config)?;

    tx.commit().context("Commit")?;

    ctx.clear_cache();
    ctx.publish_mutation_event(EventOperation::Update, "default", &doc.fields);

    Ok(doc)
}

/// Core logic for global version restore on an existing connection/transaction.
pub(crate) fn restore_global_version_core(
    ctx: &ServiceContext,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.global_def()?;

    let access = write_hooks.check_access(
        &AccessCheckInput::builder("restore", ctx.slug)
            .access(def.access.update.as_ref())
            .user(ctx.user)
            .build(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
            ctx.slug
        )));
    }

    // Restore also requires version-history access (explicit `versions` toggle;
    // an unset toggle is already covered by the `update` check above).
    check_restore_versions_gate(ctx, write_hooks, None)?;

    let gtable = global_table(ctx.slug);

    let version = query::find_version_by_id(conn, &gtable, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    // Restore to the snapshot's own publication status (see the collection
    // variant above) rather than force-publishing.
    let restored_status = version.status.clone();
    let mut snapshot = version.snapshot;

    warn_on_snapshot_drift(&snapshot, &def.fields, ctx.slug, version_id);

    // Field-level write access also gates restore — see the collection variant
    // above. Drop write-denied fields from the snapshot before validation and
    // persistence so a restore can't overwrite a write-locked field's value.
    write_hooks.strip_write_access_value(&def.fields, &mut snapshot, ctx.slug, ctx.user, None);

    // Re-run schema validation against the restored data — see the
    // collection variant above for the full rationale.
    let validation_data = snapshot_to_validation_data(&snapshot);
    let val_ctx = ValidationCtx::builder(conn, &gtable).user(ctx.user).build();
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

    write_hooks.strip_read_access_doc(&def.fields, &mut doc, ctx.slug, ctx.user, None);
    doc.strip_fields(&helpers::collect_api_hidden_field_names(&def.fields, ""));

    Ok(doc)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::{collect_known_keys, warn_on_snapshot_drift};
    use crate::core::{FieldDefinition, FieldType};

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
