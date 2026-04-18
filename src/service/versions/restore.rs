//! Version restore operations for collections and globals.

use std::collections::HashSet;

use anyhow::Context as _;
use serde_json::Value;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{Document, FieldDefinition, FieldType},
    db::{AccessResult, query, query::helpers::global_table},
    service::{RunnerWriteHooks, ServiceContext, ServiceError, helpers},
};

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
                let new_prefix = if prefix.is_empty() {
                    f.name.clone()
                } else {
                    format!("{}__{}", prefix, f.name)
                };
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
                let key = if prefix.is_empty() {
                    f.name.clone()
                } else {
                    format!("{}__{}", prefix, f.name)
                };
                out.insert(key.clone());
                // Bare name also accepted by the snapshot extractor.
                out.insert(f.name.clone());

                if f.field_type == FieldType::Date && f.timezone {
                    out.insert(format!("{key}_tz"));
                    out.insert(format!("{}_tz", f.name));
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
    let Some(obj) = snapshot.as_object() else {
        return;
    };

    let mut known: HashSet<String> = HashSet::new();
    collect_known_keys(fields, "", &mut known);

    // Accept standard document metadata + locale suffixes transparently.
    const METADATA: &[&str] = &[
        "id",
        "created_at",
        "updated_at",
        "_status",
        "_trashed_at",
        "_ref_count",
    ];

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
pub fn restore_collection_version(
    ctx: &ServiceContext,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.collection_def();
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::collection(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .cache(ctx.cache.clone())
        .build();

    let doc = restore_collection_version_core(&inner_ctx, document_id, version_id, locale_config)?;
    tx.commit().context("Commit")?;
    Ok(doc)
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
    let def = ctx.collection_def();

    let access = write_hooks.check_access(
        def.access.update.as_deref(),
        ctx.user,
        Some(document_id),
        None,
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    // Row-level enforcement for Constrained: target row must match the filters.
    helpers::enforce_access_constraints(ctx, document_id, &access, "Update", false)?;

    let version = query::find_version_by_id(conn, ctx.slug, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    warn_on_snapshot_drift(&version.snapshot, &def.fields, ctx.slug, version_id);

    let mut doc = query::restore_version(
        conn,
        ctx.slug,
        def,
        document_id,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(helpers::collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

    Ok(doc)
}

/// Restore a global document to a specific version snapshot.
pub fn restore_global_version(
    ctx: &ServiceContext,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let pool = ctx.pool.context("pool required")?;
    let runner = ctx.runner()?;
    let def = ctx.global_def();
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let mut wh = RunnerWriteHooks::new(runner).with_conn(&tx);

    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let inner_ctx = ServiceContext::global(ctx.slug, def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(ctx.user)
        .override_access(ctx.override_access)
        .cache(ctx.cache.clone())
        .build();

    let doc = restore_global_version_core(&inner_ctx, version_id, locale_config)?;

    tx.commit().context("Commit")?;

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
    let def = ctx.global_def();

    let access = write_hooks.check_access(def.access.update.as_deref(), ctx.user, None, None)?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    if matches!(access, AccessResult::Constrained(_)) {
        return Err(ServiceError::HookError(format!(
            "Access hook for global '{}' returned a filter table; globals don't support filter-based access — return true/false based on ctx.user fields instead.",
            ctx.slug
        )));
    }

    let gtable = global_table(ctx.slug);

    let version = query::find_version_by_id(conn, &gtable, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    warn_on_snapshot_drift(&version.snapshot, &def.fields, ctx.slug, version_id);

    let mut doc = query::restore_global_version(
        conn,
        ctx.slug,
        def,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    let mut read_denied = write_hooks.field_read_denied(&def.fields, ctx.user);
    read_denied.extend(helpers::collect_api_hidden_field_names(&def.fields, ""));

    doc.strip_fields(&read_denied);

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
    /// current schema, warn_on_snapshot_drift must emit a `warn!` for each.
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
