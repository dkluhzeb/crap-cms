//! Read outgoing refs from existing rows in the database.

use anyhow::Result;
use serde_json::Value;
use tracing::debug;

use crate::config::LocaleConfig;
use crate::core::{BlockDefinition, FieldChildren, FieldDefinition, field_children};
use crate::db::query::helpers::{join_table, prefixed_name};
use crate::db::query::join::{find_array_rows, find_block_rows};
use crate::db::query::poly_ref;
use crate::db::query::{column_is_localized, stored_columns};
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

    let read = RefRead {
        conn,
        table,
        id,
        root: fields,
        locale_config,
    };
    collect_refs(&read, fields, "", &mut refs)?;

    // A document referencing itself protects nothing: deleting it removes the
    // reference too. Counting it would block its own hard delete with a
    // "referenced by 1 document" whose back-reference list (which already
    // skips the owner) is empty. Filtered at the ONE reader every count path
    // (create replay, update diff, hard-delete, backfill) goes through, so
    // they agree.
    refs.retain(|r| !(r.target_collection == table && r.target_id == id));

    Ok(refs)
}

/// The document whose references are read, and the schema its columns follow.
struct RefRead<'a> {
    conn: &'a dyn DbConnection,
    table: &'a str,
    id: &'a str,
    root: &'a [FieldDefinition],
    locale_config: &'a LocaleConfig,
}

/// Recursively walk the field tree and collect outgoing refs.
fn collect_refs(
    read: &RefRead<'_>,
    fields: &[FieldDefinition],
    prefix: &str,
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
    for field in fields {
        // Structural dispatch via the SHARED classifier (see `compute.rs`
        // for the rationale) — the DB-read counterpart of the create-time
        // compute walker. Only the value source differs (real columns /
        // junction tables here vs the in-memory data map there).
        match field_children(field) {
            FieldChildren::Group(sub_fields) => {
                collect_refs(read, sub_fields, &prefixed_name(prefix, &field.name), refs)?;
            }
            FieldChildren::Wrapper(sub_fields) => {
                collect_refs(read, sub_fields, prefix, refs)?;
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_refs(read, &tab.fields, prefix, refs)?;
                }
            }

            FieldChildren::Array(sub_fields) => {
                let field_name = prefixed_name(prefix, &field.name);
                collect_array_refs(
                    read.conn,
                    read.table,
                    &field_name,
                    read.id,
                    sub_fields,
                    refs,
                );
            }

            FieldChildren::Blocks(block_defs) => {
                let field_name = prefixed_name(prefix, &field.name);
                collect_blocks_refs(
                    read.conn,
                    read.table,
                    &field_name,
                    read.id,
                    block_defs,
                    refs,
                );
            }

            FieldChildren::Leaf => collect_leaf_refs(read, field, prefix, refs)?,
        }
    }

    Ok(())
}

/// Collect the stored reference of a leaf. Relationship/Upload leaves carry
/// one — a junction table for has-many, the parent columns for has-one (one per
/// locale when the column is localized, by its own flag or a parent group's);
/// every other leaf stores none.
fn collect_leaf_refs(
    read: &RefRead<'_>,
    field: &FieldDefinition,
    prefix: &str,
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
    let Some(rc) = &field.relationship else {
        return Ok(());
    };
    let col = prefixed_name(prefix, &field.name);

    if !field.has_parent_column() {
        let junction = join_table(read.table, &col);

        collect_has_many_refs(
            read.conn,
            &junction,
            read.id,
            &rc.collection,
            rc.is_polymorphic(),
            refs,
        );

        return Ok(());
    }

    let localized = column_is_localized(&col, read.root).unwrap_or(false);
    let columns = stored_columns(&col, localized, read.locale_config)?;

    collect_has_one_refs(
        read.conn,
        read.table,
        read.id,
        &columns,
        &rc.collection,
        rc.is_polymorphic(),
        refs,
    )
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

    /// Regression: a has-one relationship inside a localized group was read from
    /// a bare column that doesn't exist — the group's localization was ignored —
    /// so reading the document's references failed on update, restore and hard
    /// delete.
    #[test]
    fn a_has_one_in_a_localized_group_reads_every_locale_column() {
        let mut tags = CollectionDefinition::new("tags");
        tags.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let fields = vec![
            FieldDefinition::builder("grp", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("rel", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("tags", false))
                        .build(),
                ])
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[tags, posts], &locale_en_de());
        let conn = pool.get().unwrap();
        insert_doc(&conn, "tags", "t1");
        insert_doc_with_field(&conn, "posts", "p1", "grp__rel__de", "t1");

        let refs = read_outgoing_refs(&conn, "posts", "p1", &fields, &locale_en_de()).unwrap();

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target_id, "t1");
    }

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
