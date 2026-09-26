//! The shape of a has-many junction table — its columns and primary key — and
//! the one reconcile that brings a stored junction to the shape its field
//! wants.
//!
//! A junction's primary key is part of what the field means: a polymorphic
//! relationship keys its rows by `related_collection` too (one id may name a
//! document of two collections), and a localized one by `_locale` (each locale
//! holds its own list, which may name the same document). A table created
//! before the field gained either keeps the narrower key, and a write the
//! field allows — the same id in a second locale — violates it. The reconcile
//! reads the stored key from the catalog and widens it once.

use std::collections::HashSet;

use anyhow::{Context as _, Result};
use tracing::info;

use crate::db::DbConnection;
use crate::db::migrate::helpers::column_specs::{ensure_locale_column, locale_column_definition};
use crate::db::migrate::helpers::introspection::{
    ConstraintKind, get_table_columns, table_constraints,
};
use crate::db::query::helpers::quote_ident;

/// The column holding a polymorphic row's target collection.
const RELATED_COLLECTION_DEF: &str = "related_collection TEXT NOT NULL DEFAULT ''";

/// What a junction's rows are keyed by beyond `(parent_id, related_id)`.
#[derive(Debug, Clone, Copy)]
pub(super) struct JunctionShape<'a> {
    polymorphic: bool,
    /// Set when the table keeps rows per locale: the locale rows written
    /// before the column existed belong to.
    default_locale: Option<&'a str>,
}

impl<'a> JunctionShape<'a> {
    pub(super) fn new(polymorphic: bool, default_locale: Option<&'a str>) -> Self {
        Self {
            polymorphic,
            default_locale,
        }
    }

    /// The primary key's columns, in key order.
    fn key_columns(&self) -> Vec<&'static str> {
        let mut columns = vec!["parent_id", "related_id"];

        if self.polymorphic {
            columns.push("related_collection");
        }

        if self.default_locale.is_some() {
            columns.push("_locale");
        }

        columns
    }

    /// The `CREATE TABLE` statement of a junction `table` of this shape whose
    /// rows belong to `collection_slug`.
    fn create_sql(&self, table: &str, collection_slug: &str) -> String {
        let mut columns = vec![
            format!(
                "parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE",
                quote_ident(collection_slug)
            ),
            "related_id TEXT NOT NULL".to_string(),
        ];

        if self.polymorphic {
            columns.push(RELATED_COLLECTION_DEF.to_string());
        }

        columns.push("_order INTEGER NOT NULL DEFAULT 0".to_string());

        if let Some(locale) = self.default_locale {
            columns.push(locale_column_definition(locale));
        }

        columns.push(format!("PRIMARY KEY ({})", self.key_columns().join(", ")));

        format!(
            "CREATE TABLE {} ({})",
            quote_ident(table),
            columns.join(", ")
        )
    }

    /// The expression a rebuild copies column `col` from: the stored column
    /// when the old table has it, else the value its rows mean — no target
    /// collection, the default locale.
    fn copy_expr(&self, col: &str, stored: &HashSet<String>) -> String {
        if stored.contains(col) {
            return col.to_string();
        }

        match (col, self.default_locale) {
            ("_locale", Some(locale)) => format!("'{}'", locale.replace('\'', "''")),
            _ => "''".to_string(),
        }
    }

    /// Every column of a table of this shape, in table order.
    fn columns(&self) -> Vec<&'static str> {
        let mut columns = vec!["parent_id", "related_id"];

        if self.polymorphic {
            columns.push("related_collection");
        }

        columns.push("_order");

        if self.default_locale.is_some() {
            columns.push("_locale");
        }

        columns
    }
}

/// Create a junction table of `shape`.
pub(super) fn create_junction_table(
    conn: &dyn DbConnection,
    table: &str,
    collection_slug: &str,
    shape: &JunctionShape<'_>,
) -> Result<()> {
    info!("Creating junction table: {}", table);

    conn.execute_ddl(&shape.create_sql(table, collection_slug), &[])
        .with_context(|| format!("Failed to create junction table {table}"))?;

    Ok(())
}

/// Bring an existing junction table to `shape`: when its stored primary key
/// lacks a column the shape keys by, the column is added (existing rows take
/// no target collection / the default locale) and the key widened. A key
/// wider than the shape's is left alone — every write the field makes is
/// unique under it too, and narrowing it would have to drop rows.
pub(super) fn reconcile_junction_shape(
    conn: &dyn DbConnection,
    table: &str,
    collection_slug: &str,
    shape: &JunctionShape<'_>,
) -> Result<()> {
    let stored_key: Vec<String> = table_constraints(conn, table, ConstraintKind::PrimaryKey)?
        .into_iter()
        .flat_map(|c| c.columns)
        .collect();

    let wanted = shape.key_columns();
    if wanted.iter().all(|col| stored_key.iter().any(|s| s == col)) {
        return Ok(());
    }

    info!(
        "Re-keying junction table {table} by ({}) (was ({}))",
        wanted.join(", "),
        stored_key.join(", ")
    );

    let rekeyed = if conn.is_postgres() {
        rekey_in_place(conn, table, shape)
    } else {
        rebuild(conn, table, collection_slug, shape)
    };

    rekeyed.with_context(|| format!("Failed to re-key junction table {table}"))
}

/// Postgres changes a primary key in place: add the missing columns, drop the
/// old key, add the new one.
fn rekey_in_place(conn: &dyn DbConnection, table: &str, shape: &JunctionShape<'_>) -> Result<()> {
    let existing = get_table_columns(conn, table)?;

    if shape.polymorphic && !existing.contains("related_collection") {
        conn.execute_ddl(
            &format!(
                "ALTER TABLE {} ADD COLUMN {RELATED_COLLECTION_DEF}",
                quote_ident(table)
            ),
            &[],
        )?;
    }

    if let Some(locale) = shape.default_locale {
        ensure_locale_column(conn, table, locale)?;
    }

    for key in table_constraints(conn, table, ConstraintKind::PrimaryKey)? {
        conn.execute_ddl(
            &format!(
                "ALTER TABLE {} DROP CONSTRAINT {}",
                quote_ident(table),
                quote_ident(&key.name)
            ),
            &[],
        )?;
    }

    conn.execute_ddl(
        &format!(
            "ALTER TABLE {} ADD PRIMARY KEY ({})",
            quote_ident(table),
            shape.key_columns().join(", ")
        ),
        &[],
    )?;

    Ok(())
}

/// `SQLite` can't change a primary key: the table is recreated in the shape
/// and its rows copied over.
fn rebuild(
    conn: &dyn DbConnection,
    table: &str,
    collection_slug: &str,
    shape: &JunctionShape<'_>,
) -> Result<()> {
    let stored = get_table_columns(conn, table)?;
    let temp = format!("_{table}_migrate");

    conn.execute_batch_ddl(&format!(
        "ALTER TABLE {} RENAME TO {}",
        quote_ident(table),
        quote_ident(&temp)
    ))?;

    conn.execute_batch_ddl(&shape.create_sql(table, collection_slug))?;

    let columns = shape.columns();
    let exprs: Vec<String> = columns
        .iter()
        .map(|col| shape.copy_expr(col, &stored))
        .collect();

    conn.execute_batch(&format!(
        "INSERT INTO {} ({}) SELECT {} FROM {}",
        quote_ident(table),
        columns.join(", "),
        exprs.join(", "),
        quote_ident(&temp)
    ))?;

    conn.execute_batch_ddl(&format!("DROP TABLE {}", quote_ident(&temp)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldType, RelationshipConfig};
    use crate::db::migrate::collection::{create_collection_table, test_helpers::*};
    use crate::db::migrate::helpers::join_tables::sync_join_tables;
    use crate::db::query::{find_related_ids, set_related_ids};

    fn tags(localized: bool) -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .localized(localized)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ]
    }

    fn stored_key(conn: &dyn DbConnection, table: &str) -> Vec<String> {
        table_constraints(conn, table, ConstraintKind::PrimaryKey)
            .unwrap()
            .into_iter()
            .flat_map(|c| c.columns)
            .collect()
    }

    /// Regression: a has-many junction that gained `_locale` after it existed
    /// kept its `(parent_id, related_id)` key, so the second locale's list
    /// could not name a document the first one held. The sync re-keys it by
    /// locale once; existing rows belong to the default locale.
    #[test]
    fn a_junction_turned_localized_holds_the_same_id_per_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", tags(false));
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string()], None).unwrap();

        sync_join_tables(&conn, "posts", &tags(true), &locale_en_de()).unwrap();
        assert_eq!(
            stored_key(&conn, "posts_tags"),
            vec!["parent_id", "related_id", "_locale"]
        );

        set_related_ids(
            &conn,
            "posts",
            "tags",
            "p1",
            &["t1".to_string()],
            Some("de"),
        )
        .expect("the same id in a second locale is a distinct row");

        for locale in ["en", "de"] {
            assert_eq!(
                find_related_ids(&conn, "posts", "tags", "p1", Some(locale)).unwrap(),
                vec!["t1"],
                "{locale}"
            );
        }
    }

    /// A junction already in its shape is left alone, and a key wider than
    /// the shape's (localization turned off again) is not narrowed.
    #[test]
    fn a_junction_in_shape_is_not_rebuilt() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", tags(true));
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();
        conn.execute_batch(
            "INSERT INTO posts (id) VALUES ('p1'); \
             INSERT INTO posts_tags (parent_id, related_id, _locale) VALUES ('p1', 't1', 'de');",
        )
        .unwrap();

        let shape = JunctionShape::new(false, None);
        reconcile_junction_shape(&conn, "posts_tags", "posts", &shape).unwrap();

        assert_eq!(
            stored_key(&conn, "posts_tags"),
            vec!["parent_id", "related_id", "_locale"]
        );
        assert_eq!(
            find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap(),
            vec!["t1"],
            "no row is dropped"
        );
    }
}
