//! One-time backfill of `_ref_count` columns from existing relationship data.

use anyhow::{Context as _, Result};
use tracing::{debug, info, warn};

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, FieldType, Registry, field::flatten_array_sub_fields},
    db::{DbConnection, DbValue},
};

const META_KEY: &str = "ref_count_backfilled";

/// Run the ref count backfill if not already done.
/// Must be called within a transaction after tables have been synced.
pub fn backfill_if_needed(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let p1 = conn.placeholder(1);
    let row = conn.query_one(
        &format!("SELECT value FROM _crap_meta WHERE key = {p1}"),
        &[DbValue::Text(META_KEY.to_string())],
    )?;

    if row.is_some() {
        return Ok(());
    }

    info!("Backfilling _ref_count columns from existing relationship data...");

    // Reset all ref_counts to 0 first
    for slug in registry.collections.keys() {
        conn.execute(&format!("UPDATE \"{}\" SET _ref_count = 0", slug), &[])?;
    }

    for slug in registry.globals.keys() {
        conn.execute(
            &format!("UPDATE \"_global_{}\" SET _ref_count = 0", slug),
            &[],
        )?;
    }

    // Walk each collection and global's fields, counting outgoing refs
    for (slug, def) in &registry.collections {
        backfill_collection(conn, slug, &def.fields, locale_config)?;
    }

    for (slug, def) in &registry.globals {
        let table = format!("_global_{}", slug);
        backfill_collection(conn, &table, &def.fields, locale_config)?;
    }

    // Mark as done
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!("INSERT INTO _crap_meta (key, value) VALUES ({p1}, {p2})"),
        &[
            DbValue::Text(META_KEY.to_string()),
            DbValue::Text("true".to_string()),
        ],
    )?;

    info!("Ref count backfill complete");
    Ok(())
}

/// Backfill ref counts for one collection/global table.
fn backfill_collection(
    conn: &dyn DbConnection,
    table: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    backfill_fields(conn, table, fields, locale_config, "")
}

/// Build a prefixed column/table name: `"prefix__name"` or just `"name"`.
fn prefixed(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}__{}", prefix, name)
    }
}

fn backfill_fields(
    conn: &dyn DbConnection,
    table: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    prefix: &str,
) -> Result<()> {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                backfill_fields(
                    conn,
                    table,
                    &field.fields,
                    locale_config,
                    &prefixed(prefix, &field.name),
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                backfill_fields(conn, table, &field.fields, locale_config, prefix)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    backfill_fields(conn, table, &tab.fields, locale_config, prefix)?;
                }
            }

            FieldType::Relationship | FieldType::Upload => {
                let rc = match &field.relationship {
                    Some(rc) => rc,
                    None => continue,
                };

                let col = prefixed(prefix, &field.name);

                if field.has_parent_column() {
                    backfill_has_one(
                        conn,
                        table,
                        &col,
                        &rc.collection,
                        rc.is_polymorphic(),
                        field.localized && locale_config.is_enabled(),
                        locale_config,
                    )?;
                } else {
                    let junction = format!("{}_{}", table, col);
                    backfill_has_many(conn, &junction, &rc.collection, rc.is_polymorphic())?;
                }
            }

            FieldType::Array => {
                let array_table = format!("{}_{}", table, prefixed(prefix, &field.name));
                backfill_array(conn, &array_table, &field.fields)?;
            }

            FieldType::Blocks => {
                let blocks_table = format!("{}_{}", table, prefixed(prefix, &field.name));
                backfill_blocks(conn, &blocks_table, &field.blocks)?;
            }

            _ => {}
        }
    }

    Ok(())
}

/// Query grouped values from a column and increment ref counts on targets.
fn backfill_column_refs(
    conn: &dyn DbConnection,
    table: &str,
    col_name: &str,
    default_collection: &str,
    is_polymorphic: bool,
) -> Result<()> {
    let sql = format!(
        "SELECT \"{}\", COUNT(*) FROM \"{}\" WHERE \"{}\" IS NOT NULL AND \"{}\" != '' GROUP BY \"{}\"",
        col_name, table, col_name, col_name, col_name
    );

    let rows = match conn.query_all(&sql, &[]) {
        Ok(r) => r,
        Err(e) => {
            warn!("Backfill skipping {}.{}: {}", table, col_name, e);
            return Ok(());
        }
    };

    for row in &rows {
        let value = match row.get_value(0) {
            Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };
        let count = match row.get_value(1) {
            Some(DbValue::Integer(n)) => *n,
            _ => continue,
        };

        if is_polymorphic {
            if let Some((target_col, target_id)) = value.split_once('/')
                && !target_col.is_empty()
                && !target_id.is_empty()
            {
                increment_ref_count(conn, target_col, target_id, count)?;
            }
        } else {
            increment_ref_count(conn, default_collection, &value, count)?;
        }
    }

    Ok(())
}

/// Backfill has-one: for each distinct non-null value in the column, increment target's ref count.
fn backfill_has_one(
    conn: &dyn DbConnection,
    table: &str,
    col: &str,
    default_collection: &str,
    is_polymorphic: bool,
    is_localized: bool,
    locale_config: &LocaleConfig,
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

    for col_name in &columns {
        backfill_column_refs(conn, table, col_name, default_collection, is_polymorphic)?;
    }

    Ok(())
}

/// Backfill has-many: count refs in junction table and increment targets.
fn backfill_has_many(
    conn: &dyn DbConnection,
    junction_table: &str,
    default_collection: &str,
    is_polymorphic: bool,
) -> Result<()> {
    if is_polymorphic {
        backfill_polymorphic_junction(conn, junction_table)?;
    } else {
        backfill_column_refs(
            conn,
            junction_table,
            "related_id",
            default_collection,
            false,
        )?;
    }

    Ok(())
}

/// Backfill polymorphic junction table refs (related_collection + related_id pairs).
fn backfill_polymorphic_junction(conn: &dyn DbConnection, junction_table: &str) -> Result<()> {
    let sql = format!(
        "SELECT related_collection, related_id, COUNT(*) FROM \"{}\" GROUP BY related_collection, related_id",
        junction_table
    );

    let rows = match conn.query_all(&sql, &[]) {
        Ok(r) => r,
        Err(e) => {
            warn!("Backfill skipping {}: {}", junction_table, e);
            return Ok(());
        }
    };

    for row in &rows {
        let target_col = match row.get_value(0) {
            Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };
        let target_id = match row.get_value(1) {
            Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };
        let count = match row.get_value(2) {
            Some(DbValue::Integer(n)) => *n,
            _ => continue,
        };

        increment_ref_count(conn, &target_col, &target_id, count)?;
    }

    Ok(())
}

/// Backfill array sub-field refs.
fn backfill_array(
    conn: &dyn DbConnection,
    array_table: &str,
    fields: &[FieldDefinition],
) -> Result<()> {
    let flat = flatten_array_sub_fields(fields);

    for sub in &flat {
        if !matches!(sub.field_type, FieldType::Relationship | FieldType::Upload) {
            continue;
        }

        let rc = match &sub.relationship {
            Some(rc) if !rc.has_many => rc,
            _ => continue,
        };

        let sql = format!(
            "SELECT \"{}\", COUNT(*) FROM \"{}\" WHERE \"{}\" IS NOT NULL AND \"{}\" != '' GROUP BY \"{}\"",
            sub.name, array_table, sub.name, sub.name, sub.name
        );
        let rows = match conn.query_all(&sql, &[]) {
            Ok(r) => r,
            Err(e) => {
                warn!("Backfill skipping {}.{}: {}", array_table, sub.name, e);
                continue;
            }
        };

        for row in &rows {
            let value = match row.get_value(0) {
                Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
                _ => continue,
            };
            let count = match row.get_value(1) {
                Some(DbValue::Integer(n)) => *n,
                _ => continue,
            };

            if rc.is_polymorphic() {
                if let Some((col, id)) = value.split_once('/')
                    && !col.is_empty()
                    && !id.is_empty()
                {
                    increment_ref_count(conn, col, id, count)?;
                }
            } else {
                increment_ref_count(conn, &rc.collection, &value, count)?;
            }
        }
    }

    Ok(())
}

/// Backfill blocks sub-field refs.
fn backfill_blocks(
    conn: &dyn DbConnection,
    blocks_table: &str,
    blocks: &[crate::core::BlockDefinition],
) -> Result<()> {
    for block in blocks {
        let flat = flatten_array_sub_fields(&block.fields);

        for sub in &flat {
            if !matches!(sub.field_type, FieldType::Relationship | FieldType::Upload) {
                continue;
            }

            let rc = match &sub.relationship {
                Some(rc) if !rc.has_many => rc,
                _ => continue,
            };

            let extract = conn.json_extract_expr("data", &sub.name);
            let p1 = conn.placeholder(1);
            let sql = format!(
                "SELECT {}, COUNT(*) FROM \"{}\" WHERE _block_type = {p1} AND {} IS NOT NULL AND {} != '' GROUP BY {}",
                extract, blocks_table, extract, extract, extract
            );
            let rows = match conn.query_all(&sql, &[DbValue::Text(block.block_type.clone())]) {
                Ok(r) => r,
                Err(e) => {
                    debug!(
                        "Backfill skipping {}.{}.{}: {}",
                        blocks_table, block.block_type, sub.name, e
                    );
                    continue;
                }
            };

            for row in &rows {
                let value = match row.get_value(0) {
                    Some(DbValue::Text(s)) if !s.is_empty() => s.clone(),
                    _ => continue,
                };
                let count = match row.get_value(1) {
                    Some(DbValue::Integer(n)) => *n,
                    _ => continue,
                };

                if rc.is_polymorphic() {
                    if let Some((col, id)) = value.split_once('/')
                        && !col.is_empty()
                        && !id.is_empty()
                    {
                        increment_ref_count(conn, col, id, count)?;
                    }
                } else {
                    increment_ref_count(conn, &rc.collection, &value, count)?;
                }
            }
        }
    }

    Ok(())
}

/// Increment _ref_count on a target document by the given amount.
fn increment_ref_count(
    conn: &dyn DbConnection,
    collection: &str,
    id: &str,
    count: i64,
) -> Result<()> {
    let p1 = conn.placeholder(1);
    let p2 = conn.placeholder(2);
    let sql = format!(
        "UPDATE \"{}\" SET _ref_count = _ref_count + {p2} WHERE id = {p1}",
        collection
    );
    conn.execute(
        &sql,
        &[DbValue::Text(id.to_string()), DbValue::Integer(count)],
    )
    .with_context(|| format!("Failed to increment _ref_count on {}/{}", collection, id))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig};
    use crate::core::Slug;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::migrate::collection::test_helpers::no_locale;
    use crate::db::{DbConnection, DbPool, migrate, pool};

    fn setup_db(
        collections: &[CollectionDefinition],
        globals: &[GlobalDefinition],
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

        let registry_shared = crate::core::Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            for c in collections {
                reg.register_collection(c.clone());
            }
            for g in globals {
                reg.register_global(g.clone());
            }
        }
        migrate::sync_all(&db_pool, &registry_shared, locale).expect("sync");

        let registry = (*crate::core::Registry::snapshot(&registry_shared)).clone();
        (tmp, db_pool, registry)
    }

    fn get_ref_count(conn: &dyn DbConnection, table: &str, id: &str) -> i64 {
        crate::db::query::ref_count::get_ref_count(conn, table, id)
            .unwrap()
            .expect("document should exist")
    }

    // ── Basic backfill ───────────────────────────────────────────────────

    #[test]
    fn backfill_has_one_relationships() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        // Insert data bypassing ref counting
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m2')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p2', 'm1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p3', 'm2')", &[])
            .unwrap();

        // Clear the backfill flag so it runs again
        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 2);
        assert_eq!(get_ref_count(&conn, "media", "m2"), 1);
    }

    // ── Has-many backfill ────────────────────────────────────────────────

    #[test]
    fn backfill_has_many_relationships() {
        let tags = CollectionDefinition::new("tags");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[tags, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO tags (id) VALUES ('t1')", &[])
            .unwrap();
        conn.execute("INSERT INTO tags (id) VALUES ('t2')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p2')", &[])
            .unwrap();
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
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p2', 't1', 0)",
            &[],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "tags", "t1"), 2);
        assert_eq!(get_ref_count(&conn, "tags", "t2"), 1);
    }

    // ── Idempotent (second run is no-op) ─────────────────────────────────

    #[test]
    fn backfill_is_idempotent() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id, image) VALUES ('p1', 'm1')", &[])
            .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();
        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);

        // Second run should be a no-op (flag is set)
        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();
        assert_eq!(
            get_ref_count(&conn, "media", "m1"),
            1,
            "Should not double-count"
        );
    }

    // ── Polymorphic has-many ─────────────────────────────────────────────

    #[test]
    fn backfill_polymorphic_has_many() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, pages, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO pages (id) VALUES ('pg1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'm1', 'media', 0)",
            &[],
        ).unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES ('p1', 'pg1', 'pages', 1)",
            &[],
        ).unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);
        assert_eq!(get_ref_count(&conn, "pages", "pg1"), 1);
    }

    // ── Global outgoing refs ─────────────────────────────────────────────

    #[test]
    fn backfill_global_outgoing_refs() {
        let media = CollectionDefinition::new("media");
        let mut settings = GlobalDefinition::new("settings");
        settings.fields = vec![
            FieldDefinition::builder("logo", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media], &[settings], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("UPDATE _global_settings SET logo = 'm1'", &[])
            .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 1);
    }

    // ── Array sub-field refs ─────────────────────────────────────────────

    #[test]
    fn backfill_array_sub_field_refs() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("image", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s1', 'p1', 0, 'm1')",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s2', 'p1', 1, 'm1')",
            &[],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM _crap_meta WHERE key = 'ref_count_backfilled'",
            &[],
        )
        .unwrap();

        backfill_if_needed(&conn, &registry, &no_locale()).unwrap();

        assert_eq!(get_ref_count(&conn, "media", "m1"), 2);
    }
}
