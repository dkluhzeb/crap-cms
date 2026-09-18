//! Restoring a version snapshot onto a global.

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{Document, GlobalDefinition, collection::VersionsConfig},
    db::{
        DbConnection,
        query::{
            LocaleContext,
            global::{get_global, update_global},
            helpers::global_table,
            ref_count,
            versions::{
                VersionWrite, create_version_and_prune, restore::row::restore_locale_and_join_data,
                set_document_status, snapshot::extract_snapshot_data,
            },
        },
    },
};

/// Write a stored snapshot back over a global's row — the global twin of
/// [`write_snapshot_base`](super::write_snapshot_base), shared by restore and
/// by publishing a global's pending draft.
///
/// # Errors
///
/// Returns a backend error if the UPDATE or the locale/join-table sync fails.
pub fn write_global_snapshot_base(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    obj: &Map<String, Value>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let gtable = global_table(slug);
    let data = extract_snapshot_data(obj, &def.fields, locale_config.is_enabled());
    let locale_ctx = LocaleContext::default_for(locale_config);

    update_global(conn, slug, def, &data, locale_ctx.as_ref())?;

    restore_locale_and_join_data(conn, &gtable, "default", &def.fields, obj, locale_config)
}

/// Restore a version snapshot back to a global's main table.
/// Group fields use expanded `field__subfield` sub-columns (same as collections).
///
/// # Errors
///
/// Returns an error if the snapshot is not a JSON object, or a backend error
/// if the UPDATE / join-table sync / version creation fails.
pub fn restore_global_version(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    snapshot: &Value,
    status: &str,
    locale_config: &LocaleConfig,
) -> Result<Document> {
    let obj = snapshot
        .as_object()
        .ok_or_else(|| anyhow!("Snapshot is not a JSON object"))?;

    let gtable = global_table(slug);
    let locale_ctx = LocaleContext::default_for(locale_config);

    conn.lock_row(&gtable, "default")?;
    let old_refs =
        ref_count::snapshot_outgoing_refs(conn, &gtable, "default", &def.fields, locale_config)?;

    write_global_snapshot_base(conn, slug, def, obj, locale_config)?;

    // Adjust ref counts based on before/after diff
    ref_count::after_update(
        conn,
        &gtable,
        "default",
        &def.fields,
        locale_config,
        &old_refs,
    )?;

    if def.has_drafts() {
        set_document_status(conn, &gtable, "default", status)?;
    }

    // Recording the restore prunes to the same cap every other version write
    // honors — repeated restores used to grow the table without bound.
    create_version_and_prune(
        conn,
        &VersionWrite::builder(&gtable, "default", status, snapshot)
            .max_versions(VersionsConfig::cap(def.versions.as_ref()))
            .build(),
    )?;

    // The restored global as it now stands, not as read mid-restore.
    get_global(conn, slug, def, locale_ctx.as_ref())
}
