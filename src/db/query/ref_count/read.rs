//! Read outgoing refs from existing rows in the database.

use anyhow::Result;
use tracing::debug;

use crate::config::LocaleConfig;
use crate::core::{BlockDefinition, FieldDefinition, FieldType, field::flatten_array_sub_fields};
use crate::db::query::helpers::{join_table, locale_column, prefixed_name};
use crate::db::{DbConnection, DbValue};

use super::outgoing_ref::{OutgoingRef, push_ref};

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
                let array_table = join_table(table, &prefixed_name(prefix, &field.name));

                collect_array_refs(conn, &array_table, id, &field.fields, refs);
            }

            FieldType::Blocks => {
                let blocks_table = join_table(table, &prefixed_name(prefix, &field.name));

                collect_blocks_refs(conn, &blocks_table, id, &field.blocks, refs);
            }

            _ => {}
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
                push_ref(refs, &format!("{col}/{id}"), true, "");
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

/// Read outgoing refs from array sub-fields (has-one relationship columns in array rows).
///
/// Query errors are swallowed for the same reason as `collect_has_many_refs`.
fn collect_array_refs(
    conn: &dyn DbConnection,
    array_table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    let flat = flatten_array_sub_fields(fields);

    // Collect relationship columns we need to read
    let rel_fields: Vec<(&FieldDefinition, bool, &str)> = flat
        .iter()
        .filter_map(|f| {
            if !matches!(f.field_type, FieldType::Relationship | FieldType::Upload) {
                return None;
            }

            let rc = f.relationship.as_ref()?;

            if rc.has_many {
                return None; // has-many inside array not supported
            }

            Some((*f, rc.is_polymorphic(), rc.collection.as_ref()))
        })
        .collect();

    if rel_fields.is_empty() {
        return;
    }

    let col_list = rel_fields
        .iter()
        .map(|(f, _, _)| format!("\"{}\"", f.name))
        .collect::<Vec<_>>()
        .join(", ");

    let p1 = conn.placeholder(1);
    let sql = format!("SELECT {col_list} FROM \"{array_table}\" WHERE parent_id = {p1}");

    let rows = match conn.query_all(&sql, &[DbValue::Text(parent_id.to_string())]) {
        Ok(r) => r,
        Err(e) => {
            debug!("Ref count scan skipping {}: {}", array_table, e);

            return;
        }
    };

    for row in &rows {
        for (i, (_, is_poly, default_col)) in rel_fields.iter().enumerate() {
            if let Some(value) = row.text_at(i) {
                push_ref(refs, value, *is_poly, default_col);
            }
        }
    }
}

/// Read outgoing refs from blocks sub-fields (relationship values in JSON data).
///
/// Query errors are swallowed for the same reason as `collect_has_many_refs`.
fn collect_blocks_refs(
    conn: &dyn DbConnection,
    blocks_table: &str,
    parent_id: &str,
    blocks: &[BlockDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    for block in blocks {
        let flat = flatten_array_sub_fields(&block.fields);

        let rel_fields: Vec<(&FieldDefinition, bool, &str)> = flat
            .iter()
            .filter_map(|f| {
                if !matches!(f.field_type, FieldType::Relationship | FieldType::Upload) {
                    return None;
                }
                let rc = f.relationship.as_ref()?;
                if rc.has_many {
                    return None;
                }
                Some((*f, rc.is_polymorphic(), rc.collection.as_ref()))
            })
            .collect();

        if rel_fields.is_empty() {
            continue;
        }

        // Build SELECT with json_extract for each relationship field
        let select_exprs: Vec<String> = rel_fields
            .iter()
            .map(|(f, _, _)| conn.json_extract_expr("data", &f.name))
            .collect();

        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        let sql = format!(
            "SELECT {} FROM \"{}\" WHERE parent_id = {p1} AND _block_type = {p2}",
            select_exprs.join(", "),
            blocks_table
        );

        let rows = match conn.query_all(
            &sql,
            &[
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(block.block_type.clone()),
            ],
        ) {
            Ok(r) => r,
            Err(e) => {
                debug!("Ref count scan skipping {}: {}", blocks_table, e);
                continue;
            }
        };

        for row in &rows {
            for (i, (_, is_poly, default_col)) in rel_fields.iter().enumerate() {
                if let Some(value) = row.text_at(i) {
                    push_ref(refs, value, *is_poly, default_col);
                }
            }
        }
    }
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
