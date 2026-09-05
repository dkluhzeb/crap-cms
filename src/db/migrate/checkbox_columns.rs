//! One-time Postgres migration: checkbox columns BIGINT → SMALLINT.
//!
//! Checkbox fields store a 0/1 flag; on Postgres they were created as BIGINT
//! (8 bytes). New tables now get SMALLINT — this pass retypes the checkbox
//! columns of EXISTING databases so old and new tables agree (and the
//! type-mismatch startup warning stays quiet).
//!
//! Gated by a versioned `_crap_meta` value (same pattern as the ref-count
//! backfill): bump [`MIGRATION_VERSION`] to force a re-run on upgrade.
//! Postgres-only — `SQLite`'s INTEGER storage is variable-width already.
//!
//! Idempotent and safe to interrupt: each ALTER is guarded by introspection
//! (only columns whose CURRENT type is `bigint` are touched), so a partially
//! applied run simply continues on the next startup.

use anyhow::Result;
use tracing::info;

use crate::{
    core::{FieldDefinition, FieldType, Registry, flatten_array_sub_fields},
    db::{
        DbConnection,
        migrate::{
            helpers::{get_table_column_types, table_exists},
            meta,
        },
        query::helpers::{global_table, join_table, prefixed_name, walk_leaf_fields},
    },
};

/// `_crap_meta` key PREFIX tracking this migration — the gate is
/// **per-slug** (`checkbox_columns_smallint:{slug}`), not a single global
/// flag, so a collection added to the registry *after* the initial
/// migration still gets retyped on its first startup (the same
/// granularity the ref-count backfill uses; a global flag would skip
/// later-added tables — a class-D9 gap).
const META_KEY_PREFIX: &str = "checkbox_columns_smallint";

/// Current migration version, stored as each per-slug meta *value*. Bump
/// to force existing databases to re-run the pass once on upgrade.
const MIGRATION_VERSION: &str = "1";

fn meta_key(slug: &str) -> String {
    format!("{META_KEY_PREFIX}:{slug}")
}

/// Run the checkbox-column retype when needed (Postgres only, once per
/// [`MIGRATION_VERSION`] per slug).
///
/// # Errors
///
/// Returns a backend error if introspection, an ALTER, or the meta upsert
/// fails.
pub(super) fn migrate_if_needed(conn: &dyn DbConnection, registry: &Registry) -> Result<()> {
    if !conn.is_postgres() {
        return Ok(());
    }

    for (slug, def) in &registry.collections {
        migrate_one(conn, slug, slug, &def.fields)?;
    }

    for (slug, def) in &registry.globals {
        migrate_one(conn, slug, &global_table(slug), &def.fields)?;
    }

    Ok(())
}

/// Per-slug gate + retype: skip if this slug is already at the current
/// version, otherwise walk its field tree and stamp its own meta key.
fn migrate_one(
    conn: &dyn DbConnection,
    slug: &str,
    table: &str,
    fields: &[crate::core::FieldDefinition],
) -> Result<()> {
    let key = meta_key(slug);
    if meta::get(conn, &key)?.as_deref() == Some(MIGRATION_VERSION) {
        return Ok(());
    }

    migrate_field_tree(conn, table, fields)?;

    meta::upsert(conn, &key, MIGRATION_VERSION)?;
    Ok(())
}

/// Retype the checkbox columns of one definition's main table and its array
/// join tables. Blocks store rows as JSON (no checkbox columns); nested
/// composites inside rows are JSON too.
fn migrate_field_tree(
    conn: &dyn DbConnection,
    table: &str,
    fields: &[FieldDefinition],
) -> Result<()> {
    let mut main_bases: Vec<String> = Vec::new();
    let mut array_tables: Vec<(String, Vec<String>)> = Vec::new();

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        let full_name = prefixed_name(prefix, &field.name);

        match field.field_type {
            FieldType::Checkbox => main_bases.push(full_name),
            FieldType::Array => {
                let subs: Vec<String> = flatten_array_sub_fields(&field.fields)
                    .into_iter()
                    .filter(|sf| sf.field_type == FieldType::Checkbox)
                    .map(|sf| sf.name.clone())
                    .collect();
                if !subs.is_empty() {
                    array_tables.push((join_table(table, &full_name), subs));
                }
            }
            _ => {}
        }

        Ok(())
    });

    retype_columns(conn, table, &main_bases)?;
    for (join_tbl, bases) in &array_tables {
        retype_columns(conn, join_tbl, bases)?;
    }

    Ok(())
}

/// ALTER every column of `table` that (a) belongs to a checkbox base —
/// matching the base name exactly or a `base__locale` variant — and (b) is
/// CURRENTLY `bigint`. The type guard makes the pass idempotent and keeps it
/// off system columns.
fn retype_columns(conn: &dyn DbConnection, table: &str, bases: &[String]) -> Result<()> {
    if bases.is_empty() || !table_exists(conn, table)? {
        return Ok(());
    }

    let column_types = get_table_column_types(conn, table)?;

    for (col, db_type) in &column_types {
        if !db_type.eq_ignore_ascii_case("bigint") {
            continue;
        }

        // A full column name can only be followed by a `__locale` suffix
        // (field names themselves may not contain `__`), so prefix-matching
        // `base__` is unambiguous.
        let is_checkbox = bases
            .iter()
            .any(|b| col == b || col.starts_with(&format!("{b}__")));
        if !is_checkbox {
            continue;
        }

        info!("Retyping checkbox column \"{table}\".\"{col}\" BIGINT -> SMALLINT");
        conn.execute_batch(&format!(
            "ALTER TABLE \"{table}\" ALTER COLUMN \"{col}\" TYPE SMALLINT \
             USING \"{col}\"::smallint"
        ))?;
    }

    Ok(())
}
