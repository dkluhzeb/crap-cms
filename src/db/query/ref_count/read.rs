//! Read outgoing refs from existing rows in the database.

use anyhow::Result;
use serde_json::Value;
use tracing::debug;

use crate::config::LocaleConfig;
use crate::core::{BlockDefinition, FieldDefinition, FieldType};
use crate::db::query::helpers::{join_table, locale_column, prefixed_name};
use crate::db::query::join::{find_array_rows, find_block_rows};
use crate::db::query::poly_ref;
use crate::db::{DbConnection, DbValue};

use super::outgoing_ref::{OutgoingRef, push_ref};
use super::walk::{walk_block_values, walk_nested_refs};

/// Read all outgoing references from a single document.
pub(super) fn read_outgoing_refs(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<Vec<OutgoingRef>> {
    let mut refs = Vec::new();

    collect_refs(conn, table, id, fields, locale_config, "", &mut refs)?;

    Ok(refs)
}

/// Recursively walk the field tree and collect outgoing refs.
fn collect_refs(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    prefix: &str,
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);
                collect_refs(
                    conn,
                    table,
                    id,
                    &field.fields,
                    locale_config,
                    &new_prefix,
                    refs,
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_refs(conn, table, id, &field.fields, locale_config, prefix, refs)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_refs(conn, table, id, &tab.fields, locale_config, prefix, refs)?;
                }
            }

            FieldType::Relationship | FieldType::Upload => {
                let Some(rc) = &field.relationship else {
                    continue;
                };
                let col = prefixed_name(prefix, &field.name);

                if !field.has_parent_column() {
                    let junction = join_table(table, &col);

                    collect_has_many_refs(
                        conn,
                        &junction,
                        id,
                        &rc.collection,
                        rc.is_polymorphic(),
                        refs,
                    );

                    continue;
                }

                let columns = if field.localized && locale_config.is_enabled() {
                    locale_config
                        .locales
                        .iter()
                        .map(|l| locale_column(&col, l))
                        .collect::<Result<_>>()?
                } else {
                    vec![col]
                };

                collect_has_one_refs(
                    conn,
                    table,
                    id,
                    &columns,
                    &rc.collection,
                    rc.is_polymorphic(),
                    refs,
                )?;
            }

            FieldType::Array => {
                let field_name = prefixed_name(prefix, &field.name);

                collect_array_refs(conn, table, &field_name, id, &field.fields, refs);
            }

            FieldType::Blocks => {
                let field_name = prefixed_name(prefix, &field.name);

                collect_blocks_refs(conn, table, &field_name, id, &field.blocks, refs);
            }

            // Scalars store no reference; Join is a virtual reverse-lookup with
            // no stored id. Listed explicitly (not `_`) so a future ref-bearing
            // or container field type is a compile error here rather than a
            // silent ref-count miss (a delete-protection integrity bug).
            FieldType::Text
            | FieldType::Number
            | FieldType::Textarea
            | FieldType::Richtext
            | FieldType::Select
            | FieldType::Radio
            | FieldType::Checkbox
            | FieldType::Date
            | FieldType::Email
            | FieldType::Json
            | FieldType::Code
            | FieldType::Join => {}
        }
    }

    Ok(())
}

/// Read has-one reference(s) from a parent table column.
fn collect_has_one_refs(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    columns: &[String],
    default_collection: &str,
    is_polymorphic: bool,
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
    let col_list = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let p1 = conn.placeholder(1);
    let sql = format!("SELECT {col_list} FROM \"{table}\" WHERE id = {p1}");

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(id.to_string())])? else {
        return Ok(());
    };

    for i in 0..columns.len() {
        if let Some(value) = row.text_at(i) {
            push_ref(refs, value, is_polymorphic, default_collection);
        }
    }

    Ok(())
}

/// Read has-many references from a junction table.
///
/// Query errors are intentionally swallowed (logged at debug level) — a
/// missing or unreadable junction table at scan time can't poison the
/// caller's transaction, so the function infallibly returns the refs
/// it could collect.
fn collect_has_many_refs(
    conn: &dyn DbConnection,
    junction_table: &str,
    parent_id: &str,
    default_collection: &str,
    is_polymorphic: bool,
    refs: &mut Vec<OutgoingRef>,
) {
    let p1 = conn.placeholder(1);
    let params = &[DbValue::Text(parent_id.to_string())];

    if is_polymorphic {
        // DISTINCT: junction tables permit duplicate (parent_id, related_id)
        // rows when the user submits `tags = ["a", "a", "b"]`. The ref count
        // represents an edge set, not a multiset, so duplicate rows must not
        // inflate the count.
        let sql = format!(
            "SELECT DISTINCT related_id, related_collection FROM \"{junction_table}\" WHERE parent_id = {p1}"
        );
        let rows = match conn.query_all(&sql, params) {
            Ok(r) => r,
            Err(e) => {
                debug!("Ref count scan skipping {junction_table}: {e}");

                return;
            }
        };

        for row in rows {
            if let (Some(id), Some(col)) = (row.text_at(0), row.text_at(1)) {
                push_ref(refs, &poly_ref::format(col, id), true, "");
            }
        }
    } else {
        let sql =
            format!("SELECT DISTINCT related_id FROM \"{junction_table}\" WHERE parent_id = {p1}");
        let rows = match conn.query_all(&sql, params) {
            Ok(r) => r,
            Err(e) => {
                debug!("Ref count scan skipping {junction_table}: {e}");

                return;
            }
        };

        for row in rows {
            if let Some(ref_id) = row.text_at(0) {
                push_ref(refs, ref_id, false, default_collection);
            }
        }
    }
}

/// Read outgoing refs from an array field, recursing into nested composites.
///
/// Loads the array rows exactly as document hydration assembles them (each
/// row a nested-composite JSON object, every locale included), then walks
/// them with the shared recursive walker so relationships nested inside
/// groups/arrays/blocks within a row are counted. Read errors are swallowed
/// for the same reason as `collect_has_many_refs`.
fn collect_array_refs(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    let rows = match find_array_rows(conn, collection, field_name, parent_id, fields, None) {
        Ok(r) => r,
        Err(e) => {
            debug!("Ref count scan skipping array {collection}.{field_name}: {e}");

            return;
        }
    };

    for row in &rows {
        if let Value::Object(obj) = row {
            walk_nested_refs(obj, fields, refs);
        }
    }
}

/// Read outgoing refs from a blocks field, recursing into nested composites.
///
/// Loads each block's reconstructed `data` (all locales) and walks it, so
/// relationships nested inside groups/arrays within a block — and has-many
/// relationships stored as JSON arrays in `data` — are counted. Read errors
/// are swallowed for the same reason as `collect_has_many_refs`.
fn collect_blocks_refs(
    conn: &dyn DbConnection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    blocks: &[BlockDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    let rows = match find_block_rows(conn, collection, field_name, parent_id, None) {
        Ok(r) => r,
        Err(e) => {
            debug!("Ref count scan skipping blocks {collection}.{field_name}: {e}");

            return;
        }
    };

    walk_block_values(&rows, blocks, refs);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CollectionDefinition;
    use crate::core::field::*;
    use crate::db::query::ref_count::test_helpers::*;

    #[test]
    fn no_relationship_fields_yields_no_refs() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        let (_tmp, pool, _) = setup_db(&[posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "posts", "p1");

        let refs = read_outgoing_refs(
            &conn,
            "posts",
            "p1",
            &[FieldDefinition::builder("title", FieldType::Text).build()],
            &no_locale(),
        )
        .unwrap();

        assert!(refs.is_empty());
    }
}
