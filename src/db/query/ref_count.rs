//! Reference counting for delete protection.
//!
//! Tracks how many documents reference a given target via `_ref_count` columns.
//! Replaces the O(N) back-reference scan with O(1) delete-protection checks.

use std::collections::HashMap;

use anyhow::{Context as _, Result};

use crate::{
    config::LocaleConfig,
    core::{BlockDefinition, FieldDefinition, FieldType, field::flatten_array_sub_fields},
    db::{DbConnection, DbValue},
};

/// An outgoing reference from one document to another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OutgoingRef {
    target_collection: String,
    target_id: String,
}

/// Read the `_ref_count` value for a document.
/// Returns `None` if the document does not exist, `Some(count)` otherwise
/// (defaulting to 0 when the column is NULL).
pub fn get_ref_count(conn: &dyn DbConnection, collection: &str, id: &str) -> Result<Option<i64>> {
    let p1 = conn.placeholder(1);
    let sql = format!("SELECT _ref_count FROM \"{}\" WHERE id = {p1}", collection);
    let row = conn.query_one(&sql, &[DbValue::Text(id.to_string())])?;

    Ok(row.map(|r| {
        r.get_value(0)
            .and_then(|v| match v {
                DbValue::Integer(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0)
    }))
}

/// Adjust ref counts after creating a new document.
/// Reads the newly written outgoing refs and increments targets.
pub fn after_create(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    let new_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(&[], &new_refs);
    apply_deltas(conn, &deltas)
}

/// Adjust ref counts before hard-deleting a document.
/// Reads current outgoing refs and decrements targets.
/// Must be called BEFORE the DELETE (CASCADE would remove junction rows).
pub fn before_hard_delete(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    let old_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(&old_refs, &[]);
    apply_deltas(conn, &deltas)
}

/// Adjust ref counts around an update.
/// Reads outgoing refs before and after, then applies the diff.
///
/// The caller must pass `old_refs` obtained before the mutation.
pub fn after_update(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    old_refs: Vec<OutgoingRef>,
) -> Result<()> {
    let new_refs = read_outgoing_refs(conn, table, id, fields, locale_config)?;
    let deltas = to_delta_map(&old_refs, &new_refs);
    apply_deltas(conn, &deltas)
}

/// Snapshot the current outgoing refs for a document (call before mutation).
pub fn snapshot_outgoing_refs(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<Vec<OutgoingRef>> {
    read_outgoing_refs(conn, table, id, fields, locale_config)
}

// ── Internal: read outgoing refs ────────────────────────────────────────

/// Read all outgoing references from a single document.
fn read_outgoing_refs(
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
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
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
                let rc = match &field.relationship {
                    Some(rc) => rc,
                    None => continue,
                };

                let col = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };

                if field.has_parent_column() {
                    // Has-one: read column value(s) from the parent table
                    collect_has_one_refs(
                        conn,
                        table,
                        id,
                        &col,
                        &rc.collection,
                        rc.is_polymorphic(),
                        field.localized && locale_config.is_enabled(),
                        locale_config,
                        refs,
                    )?;
                } else {
                    // Has-many: read from junction table
                    let junction = format!("{}_{}", table, col);
                    collect_has_many_refs(
                        conn,
                        &junction,
                        id,
                        &rc.collection,
                        rc.is_polymorphic(),
                        refs,
                    )?;
                }
            }

            FieldType::Array => {
                let array_table = format!(
                    "{}_{}",
                    table,
                    if prefix.is_empty() {
                        field.name.clone()
                    } else {
                        format!("{}__{}", prefix, field.name)
                    }
                );
                collect_array_refs(conn, &array_table, id, &field.fields, refs)?;
            }

            FieldType::Blocks => {
                let blocks_table = format!(
                    "{}_{}",
                    table,
                    if prefix.is_empty() {
                        field.name.clone()
                    } else {
                        format!("{}__{}", prefix, field.name)
                    }
                );
                collect_blocks_refs(conn, &blocks_table, id, &field.blocks, refs)?;
            }

            _ => {}
        }
    }

    Ok(())
}

/// Read has-one reference(s) from a parent table column.
#[allow(clippy::too_many_arguments)]
fn collect_has_one_refs(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    col: &str,
    default_collection: &str,
    is_polymorphic: bool,
    is_localized: bool,
    locale_config: &LocaleConfig,
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
    let columns: Vec<String> = if is_localized {
        locale_config
            .locales
            .iter()
            .map(|l| format!("{}__{}", col, l))
            .collect()
    } else {
        vec![col.to_string()]
    };

    let col_list = columns
        .iter()
        .map(|c| format!("\"{}\"", c))
        .collect::<Vec<_>>()
        .join(", ");

    let p1 = conn.placeholder(1);
    let sql = format!("SELECT {} FROM \"{}\" WHERE id = {p1}", col_list, table);

    let row = match conn.query_one(&sql, &[DbValue::Text(id.to_string())])? {
        Some(r) => r,
        None => return Ok(()),
    };

    for (i, _) in columns.iter().enumerate() {
        let value = match row.get_value(i) {
            Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };

        if is_polymorphic {
            if let Some((col_name, ref_id)) = value.split_once('/')
                && !col_name.is_empty()
                && !ref_id.is_empty()
            {
                refs.push(OutgoingRef {
                    target_collection: col_name.to_string(),
                    target_id: ref_id.to_string(),
                });
            }
        } else {
            refs.push(OutgoingRef {
                target_collection: default_collection.to_string(),
                target_id: value,
            });
        }
    }

    Ok(())
}

/// Read has-many references from a junction table.
fn collect_has_many_refs(
    conn: &dyn DbConnection,
    junction_table: &str,
    parent_id: &str,
    default_collection: &str,
    is_polymorphic: bool,
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
    let p1 = conn.placeholder(1);

    if is_polymorphic {
        let sql = format!(
            "SELECT related_id, related_collection FROM \"{}\" WHERE parent_id = {p1}",
            junction_table
        );
        let rows = match conn.query_all(&sql, &[DbValue::Text(parent_id.to_string())]) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("Ref count scan skipping {}: {}", junction_table, e);
                return Ok(());
            }
        };

        for row in rows {
            let ref_id = match row.get_value(0) {
                Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
                _ => continue,
            };
            let ref_col = match row.get_value(1) {
                Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
                _ => continue,
            };
            refs.push(OutgoingRef {
                target_collection: ref_col,
                target_id: ref_id,
            });
        }
    } else {
        let sql = format!(
            "SELECT related_id FROM \"{}\" WHERE parent_id = {p1}",
            junction_table
        );
        let rows = match conn.query_all(&sql, &[DbValue::Text(parent_id.to_string())]) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("Ref count scan skipping {}: {}", junction_table, e);
                return Ok(());
            }
        };

        for row in rows {
            if let Some(DbValue::Text(ref_id)) = row.get_value(0)
                && !ref_id.is_empty()
            {
                refs.push(OutgoingRef {
                    target_collection: default_collection.to_string(),
                    target_id: ref_id.clone(),
                });
            }
        }
    }

    Ok(())
}

/// Read outgoing refs from array sub-fields (has-one relationship columns in array rows).
fn collect_array_refs(
    conn: &dyn DbConnection,
    array_table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
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
        return Ok(());
    }

    let col_list = rel_fields
        .iter()
        .map(|(f, _, _)| format!("\"{}\"", f.name))
        .collect::<Vec<_>>()
        .join(", ");

    let p1 = conn.placeholder(1);
    let sql = format!(
        "SELECT {} FROM \"{}\" WHERE parent_id = {p1}",
        col_list, array_table
    );

    let rows = match conn.query_all(&sql, &[DbValue::Text(parent_id.to_string())]) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Ref count scan skipping {}: {}", array_table, e);
            return Ok(());
        }
    };

    for row in &rows {
        for (i, (_, is_poly, default_col)) in rel_fields.iter().enumerate() {
            let value = match row.get_value(i) {
                Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
                _ => continue,
            };

            if *is_poly {
                if let Some((col_name, ref_id)) = value.split_once('/')
                    && !col_name.is_empty()
                    && !ref_id.is_empty()
                {
                    refs.push(OutgoingRef {
                        target_collection: col_name.to_string(),
                        target_id: ref_id.to_string(),
                    });
                }
            } else {
                refs.push(OutgoingRef {
                    target_collection: default_col.to_string(),
                    target_id: value,
                });
            }
        }
    }

    Ok(())
}

/// Read outgoing refs from blocks sub-fields (relationship values in JSON data).
fn collect_blocks_refs(
    conn: &dyn DbConnection,
    blocks_table: &str,
    parent_id: &str,
    blocks: &[BlockDefinition],
    refs: &mut Vec<OutgoingRef>,
) -> Result<()> {
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
                tracing::debug!("Ref count scan skipping {}: {}", blocks_table, e);
                continue;
            }
        };

        for row in &rows {
            for (i, (_, is_poly, default_col)) in rel_fields.iter().enumerate() {
                let value = match row.get_value(i) {
                    Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
                    _ => continue,
                };

                if *is_poly {
                    if let Some((col_name, ref_id)) = value.split_once('/')
                        && !col_name.is_empty()
                        && !ref_id.is_empty()
                    {
                        refs.push(OutgoingRef {
                            target_collection: col_name.to_string(),
                            target_id: ref_id.to_string(),
                        });
                    }
                } else {
                    refs.push(OutgoingRef {
                        target_collection: default_col.to_string(),
                        target_id: value,
                    });
                }
            }
        }
    }

    Ok(())
}

// ── Internal: compute and apply deltas ──────────────────────────────────

/// Compute ref count deltas between old and new outgoing ref sets.
fn to_delta_map(
    old_refs: &[OutgoingRef],
    new_refs: &[OutgoingRef],
) -> HashMap<(String, String), i64> {
    let mut deltas: HashMap<(String, String), i64> = HashMap::new();

    for r in old_refs {
        *deltas
            .entry((r.target_collection.clone(), r.target_id.clone()))
            .or_insert(0) -= 1;
    }

    for r in new_refs {
        *deltas
            .entry((r.target_collection.clone(), r.target_id.clone()))
            .or_insert(0) += 1;
    }

    // Remove zero-deltas
    deltas.retain(|_, v| *v != 0);
    deltas
}

/// Apply ref count deltas to target collection tables.
fn apply_deltas(conn: &dyn DbConnection, deltas: &HashMap<(String, String), i64>) -> Result<()> {
    for ((collection, id), delta) in deltas {
        let p1 = conn.placeholder(1);
        let clamped = conn.greatest_expr("0", &format!("_ref_count + ({})", delta));
        let sql = format!(
            "UPDATE \"{}\" SET _ref_count = {} WHERE id = {p1}",
            collection, clamped
        );

        let affected = conn
            .execute(&sql, &[DbValue::Text(id.clone())])
            .with_context(|| {
                format!(
                    "Failed to update _ref_count on {}.{} by {}",
                    collection, id, delta
                )
            })?;

        if affected == 0 && *delta > 0 {
            tracing::warn!(
                "Ref count target {}/{} not found — increment by {} lost (possibly deleted)",
                collection,
                id,
                delta
            );
        }

        if *delta < 0 {
            tracing::trace!(
                "Decremented _ref_count on {}/{} by {}",
                collection,
                id,
                delta.abs()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig, LocaleConfig};
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::core::{Registry, Slug};
    use crate::db::{DbConnection, DbPool, DbValue, migrate, pool};

    fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    fn locale_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn setup_db(
        collections: &[CollectionDefinition],
        locale: &LocaleConfig,
    ) -> (tempfile::TempDir, DbPool, Registry) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            for c in collections {
                reg.register_collection(c.clone());
            }
        }
        migrate::sync_all(&db_pool, &registry_shared, locale).expect("sync");

        let registry = (*Registry::snapshot(&registry_shared)).clone();
        (tmp, db_pool, registry)
    }

    fn insert_doc(conn: &dyn DbConnection, table: &str, id: &str) {
        conn.execute(
            &format!("INSERT INTO \"{}\" (id) VALUES (?1)", table),
            &[DbValue::Text(id.to_string())],
        )
        .unwrap();
    }

    fn insert_doc_with_field(conn: &dyn DbConnection, table: &str, id: &str, col: &str, val: &str) {
        conn.execute(
            &format!(
                "INSERT INTO \"{}\" (id, \"{}\") VALUES (?1, ?2)",
                table, col
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(val.to_string()),
            ],
        )
        .unwrap();
    }

    fn get_ref_count_val(conn: &dyn DbConnection, table: &str, id: &str) -> i64 {
        get_ref_count(conn, table, id)
            .unwrap()
            .expect("document should exist")
    }

    // ── get_ref_count ────────────────────────────────────────────────────

    #[test]
    fn ref_count_defaults_to_zero() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    /// Regression: get_ref_count must return None for missing documents
    /// instead of 0, so callers can distinguish "not found" from "zero refs".
    #[test]
    fn ref_count_returns_none_for_missing_document() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        let result = get_ref_count(&conn, "media", "nonexistent").unwrap();
        assert_eq!(result, None, "Missing document should return None");
    }

    // ── to_delta_map ─────────────────────────────────────────────────────

    #[test]
    fn delta_map_add_refs() {
        let new = vec![
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m1".into(),
            },
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m2".into(),
            },
        ];
        let deltas = to_delta_map(&[], &new);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&1));
        assert_eq!(deltas.get(&("media".into(), "m2".into())), Some(&1));
    }

    #[test]
    fn delta_map_remove_refs() {
        let old = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let deltas = to_delta_map(&old, &[]);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&-1));
    }

    #[test]
    fn delta_map_swap_refs() {
        let old = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let new = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m2".into(),
        }];
        let deltas = to_delta_map(&old, &new);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&-1));
        assert_eq!(deltas.get(&("media".into(), "m2".into())), Some(&1));
    }

    #[test]
    fn delta_map_no_change() {
        let refs = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let deltas = to_delta_map(&refs, &refs);
        assert!(deltas.is_empty());
    }

    #[test]
    fn delta_map_duplicate_refs() {
        let old = vec![
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m1".into(),
            },
            OutgoingRef {
                target_collection: "media".into(),
                target_id: "m1".into(),
            },
        ];
        let new = vec![OutgoingRef {
            target_collection: "media".into(),
            target_id: "m1".into(),
        }];
        let deltas = to_delta_map(&old, &new);
        assert_eq!(deltas.get(&("media".into(), "m1".into())), Some(&-1));
    }

    // ── after_create / before_hard_delete integration ────────────────────

    fn upload_field() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ]
    }

    #[test]
    fn after_create_increments_has_one() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

    #[test]
    fn before_hard_delete_decrements_has_one() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        conn.execute("UPDATE media SET _ref_count = 1 WHERE id = 'm1'", &[])
            .unwrap();

        before_hard_delete(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    #[test]
    fn ref_count_does_not_go_negative() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        before_hard_delete(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    // ── Has-many relationship ────────────────────────────────────────────

    #[test]
    fn after_create_increments_has_many() {
        let tags = CollectionDefinition::new("tags");
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[tags, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "tags", "t1");
        insert_doc(&conn, "tags", "t2");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 't1', 0)",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 't2', 1)",
            &[],
        )
        .unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 1);
        assert_eq!(get_ref_count_val(&conn, "tags", "t2"), 1);
    }

    // ── Polymorphic has-one ──────────────────────────────────────────────

    #[test]
    fn after_create_polymorphic_has_one() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let fields = vec![
            FieldDefinition::builder("featured", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, pages, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "featured", "media/m1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

    // ── Polymorphic has-many ─────────────────────────────────────────────

    #[test]
    fn after_create_polymorphic_has_many() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let fields = vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, pages, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "pages", "pg1");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'm1', 'media', 0)",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'pg1', 'pages', 1)",
            &[],
        ).unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "pages", "pg1"), 1);
    }

    // ── Localized has-one ────────────────────────────────────────────────

    #[test]
    fn after_create_localized_has_one() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("hero", FieldType::Upload)
                .localized(true)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let locale = locale_en_de();

        let (_tmp, pool, _) = setup_db(&[media, posts], &locale);
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "media", "m2");

        conn.execute(
            "INSERT INTO posts (id, hero__en, hero__de) VALUES ('p1', 'm1', 'm2')",
            &[],
        )
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("hero", FieldType::Upload)
                .localized(true)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        after_create(&conn, "posts", "p1", &fields, &locale).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "media", "m2"), 1);
    }

    // ── Array sub-field refs ─────────────────────────────────────────────

    #[test]
    fn after_create_array_sub_field_refs() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("image", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "media", "m2");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s1', 'p1', 0, 'm1')",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s2', 'p1', 1, 'm2')",
            &[],
        )
        .unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "media", "m2"), 1);
    }

    // ── Block sub-field refs ─────────────────────────────────────────────

    #[test]
    fn after_create_blocks_sub_field_refs() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![
                        FieldDefinition::builder("bg_image", FieldType::Upload)
                            .relationship(RelationshipConfig::new("media", false))
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_content (id, parent_id, _order, _block_type, data) VALUES ('b1', 'p1', 0, 'hero', '{\"bg_image\":\"m1\"}')",
            &[],
        ).unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

    // ── Group nesting ────────────────────────────────────────────────────

    #[test]
    fn after_create_group_nested_ref() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("hero", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "meta__hero", "m1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
    }

    // ── Update (swap ref) ────────────────────────────────────────────────

    #[test]
    fn after_update_swaps_ref_counts() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "media", "m2");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        // Simulate: set m1 ref_count to 1
        conn.execute("UPDATE media SET _ref_count = 1 WHERE id = 'm1'", &[])
            .unwrap();

        // Snapshot before update
        let old_refs = snapshot_outgoing_refs(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        // Simulate update: change image from m1 to m2
        conn.execute("UPDATE posts SET image = 'm2' WHERE id = 'p1'", &[])
            .unwrap();

        after_update(&conn, "posts", "p1", &fields, &no_locale(), old_refs).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
        assert_eq!(get_ref_count_val(&conn, "media", "m2"), 1);
    }

    // ── Multiple fields referencing same target ──────────────────────────

    #[test]
    fn multiple_fields_same_target() {
        let media = CollectionDefinition::new("media");
        let fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
            FieldDefinition::builder("thumbnail", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        conn.execute(
            "INSERT INTO posts (id, image, thumbnail) VALUES ('p1', 'm1', 'm1')",
            &[],
        )
        .unwrap();

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        // Same target referenced by two fields = ref_count 2
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 2);
    }

    // ── No relationship fields (empty case) ──────────────────────────────

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

    // ── Empty/null has-one column ────────────────────────────────────────

    #[test]
    fn empty_has_one_yields_no_refs() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        // Insert post with NULL image (no value provided)
        insert_doc(&conn, "posts", "p1");

        after_create(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        // No ref should be created for NULL/empty
        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    // ── apply_deltas with nonexistent target ─────────────────────────────

    #[test]
    fn apply_deltas_nonexistent_target_does_not_error() {
        let media = CollectionDefinition::new("media");
        let (_tmp, pool, _) = setup_db(&[media], &no_locale());
        let conn = pool.get().unwrap();

        // Increment ref on a target that doesn't exist — should not error
        let mut deltas = HashMap::new();
        deltas.insert(("media".to_string(), "nonexistent".to_string()), 1i64);

        let result = apply_deltas(&conn, &deltas);
        assert!(result.is_ok());
    }

    // ── apply_deltas with mixed increments and decrements ────────────────

    #[test]
    fn apply_deltas_mixed_inc_dec() {
        let media = CollectionDefinition::new("media");
        let tags = CollectionDefinition::new("tags");
        let (_tmp, pool, _) = setup_db(&[media, tags], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "tags", "t1");

        // Set initial ref counts
        conn.execute("UPDATE media SET _ref_count = 3 WHERE id = 'm1'", &[])
            .unwrap();
        conn.execute("UPDATE tags SET _ref_count = 0 WHERE id = 't1'", &[])
            .unwrap();

        let mut deltas = HashMap::new();
        deltas.insert(("media".to_string(), "m1".to_string()), -2i64);
        deltas.insert(("tags".to_string(), "t1".to_string()), 1i64);

        apply_deltas(&conn, &deltas).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 1);
    }

    // ── after_update clearing a reference ────────────────────────────────

    #[test]
    fn after_update_clearing_ref_decrements() {
        let media = CollectionDefinition::new("media");
        let fields = upload_field();
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[media, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");

        conn.execute("UPDATE media SET _ref_count = 1 WHERE id = 'm1'", &[])
            .unwrap();

        // Snapshot before update
        let old_refs = snapshot_outgoing_refs(&conn, "posts", "p1", &fields, &no_locale()).unwrap();
        assert_eq!(old_refs.len(), 1);

        // Clear the reference
        conn.execute("UPDATE posts SET image = '' WHERE id = 'p1'", &[])
            .unwrap();

        after_update(&conn, "posts", "p1", &fields, &no_locale(), old_refs).unwrap();

        assert_eq!(get_ref_count_val(&conn, "media", "m1"), 0);
    }

    // ── before_hard_delete with has-many ──────────────────────────────────

    #[test]
    fn before_hard_delete_decrements_has_many() {
        let tags = CollectionDefinition::new("tags");
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = fields.clone();

        let (_tmp, pool, _) = setup_db(&[tags, posts], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "tags", "t1");
        insert_doc(&conn, "tags", "t2");
        insert_doc(&conn, "posts", "p1");

        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 't1', 0)",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 't2', 1)",
            &[],
        )
        .unwrap();

        conn.execute("UPDATE tags SET _ref_count = 1 WHERE id = 't1'", &[])
            .unwrap();
        conn.execute("UPDATE tags SET _ref_count = 1 WHERE id = 't2'", &[])
            .unwrap();

        before_hard_delete(&conn, "posts", "p1", &fields, &no_locale()).unwrap();

        assert_eq!(get_ref_count_val(&conn, "tags", "t1"), 0);
        assert_eq!(get_ref_count_val(&conn, "tags", "t2"), 0);
    }
}
