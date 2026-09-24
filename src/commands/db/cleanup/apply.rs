//! Applying a cleanup report: drop the orphan columns, delete the stale
//! locale rows, drop the orphan tables, and recount every reference — all in
//! one transaction.

use anyhow::{Context as _, Result, bail};

use super::scan::CleanupReport;
use crate::{
    config::LocaleConfig,
    core::Registry,
    db::{
        BoxedConnection, DbConnection,
        migrate::{self, OrphanTable},
        query::{self, helpers::quote_ident},
    },
};

/// What a committed cleanup changed: the dropped columns (`table.column`),
/// the rows deleted per table, and the dropped tables.
///
/// Every field is required and the struct is built in one place, so it is
/// constructed as a plain literal.
pub(super) struct CleanupOutcome {
    pub(super) dropped_columns: Vec<String>,
    pub(super) deleted_rows: Vec<(String, usize)>,
    pub(super) dropped_tables: Vec<String>,
}

/// Drop the identified orphan columns, returning each as `table.column`.
fn drop_orphan_columns(
    conn: &dyn DbConnection,
    orphans: &[(String, Vec<String>)],
) -> Result<Vec<String>> {
    if orphans.is_empty() {
        return Ok(Vec::new());
    }

    if !conn.supports_drop_column() {
        bail!(
            "Database does not support DROP COLUMN. \
             Consider recreating the table manually."
        );
    }

    let mut dropped = Vec::new();

    for (table, cols) in orphans {
        for col in cols {
            let sql = format!("ALTER TABLE \"{table}\" DROP COLUMN \"{col}\"");

            conn.execute(&sql, &[])
                .with_context(|| format!("Failed to drop column {table}.{col}"))?;

            dropped.push(format!("{table}.{col}"));
        }
    }

    Ok(dropped)
}

/// Drop the tables no definition accounts for, returning their names.
///
/// Only ever reached with both `--drop-tables` and `--confirm`: dropping one
/// destroys the last copy of whatever the removed definition stored.
fn drop_orphan_tables(conn: &dyn DbConnection, tables: &[OrphanTable]) -> Result<Vec<String>> {
    // A junction table's `parent_id` references its collection, and Postgres
    // refuses to drop a table another one references.
    let cascade = if conn.is_postgres() { " CASCADE" } else { "" };

    let mut dropped = Vec::new();

    for table in tables {
        let sql = format!("DROP TABLE IF EXISTS {}{cascade}", quote_ident(&table.name));

        conn.execute_ddl(&sql, &[])
            .with_context(|| format!("Failed to drop table {}", table.name))?;

        dropped.push(table.name.clone());
    }

    Ok(dropped)
}

/// Delete the junction rows whose locale is no longer configured, returning
/// the number deleted per table.
fn delete_stale_locale_rows(
    conn: &dyn DbConnection,
    stale: &[(String, i64)],
    locale_config: &LocaleConfig,
) -> Result<Vec<(String, usize)>> {
    let mut deleted_rows = Vec::new();

    for (table, _) in stale {
        let deleted = query::delete_rows_outside_locales(conn, table, &locale_config.locales)
            .with_context(|| format!("Failed to delete stale locale rows from {table}"))?;

        deleted_rows.push((table.clone(), deleted));
    }

    Ok(deleted_rows)
}

/// Apply the report inside the caller's transaction — drop the orphan columns,
/// delete the stale locale rows, drop the orphan tables it lists — and recount
/// every reference in the same transaction.
///
/// A dropped column, a deleted junction row or a dropped table can have held
/// references; recounting before the commit means no count outlives the rows
/// it was made of (a phantom count blocks the target's delete forever), and a
/// failure anywhere leaves the database exactly as it was.
///
/// Nothing is printed here: the outcome is only reported once the caller's
/// transaction has committed, so a failed recount or commit never leaves
/// "Dropped" lines on screen for changes that were rolled back.
fn apply(
    conn: &dyn DbConnection,
    report: &CleanupReport,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<CleanupOutcome> {
    let dropped_columns = drop_orphan_columns(conn, &report.columns)?;
    let deleted_rows = delete_stale_locale_rows(conn, &report.stale_locale_rows, locale_config)?;
    let dropped_tables = drop_orphan_tables(conn, &report.tables)?;

    migrate::recompute_ref_counts(conn, registry, locale_config)
        .context("Failed to recompute reference counts")?;

    Ok(CleanupOutcome {
        dropped_columns,
        deleted_rows,
        dropped_tables,
    })
}

/// Apply the report in one IMMEDIATE transaction and commit it.
///
/// Returns what changed only once the commit succeeded; any failure rolls the
/// whole cleanup back and returns the error with no outcome to report.
///
/// # Errors
///
/// Returns an error if the transaction cannot begin, any step of the cleanup
/// or the reference recount fails, or the commit fails.
pub(super) fn apply_in_transaction(
    conn: &mut BoxedConnection,
    report: &CleanupReport,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<CleanupOutcome> {
    let tx = conn
        .transaction_immediate()
        .context("Failed to begin the cleanup transaction")?;

    let outcome = apply(&tx, report, registry, locale_config)?;

    tx.commit().context("Failed to commit the cleanup")?;

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::*;
    use crate::{
        commands::db::cleanup::{
            scan::{find_stale_locale_rows, scan},
            test_support::{
                locale_en_de, localized_array, make_conn, simple_collection, text_field,
            },
        },
        config::CrapConfig,
        core::{FieldDefinition, FieldType, RelationshipConfig},
        db::pool,
    };

    fn ref_count_of(conn: &dyn DbConnection, table: &str, id: &str) -> i64 {
        query::ref_count::get_ref_count(conn, table, id)
            .unwrap()
            .expect("the document exists")
    }

    fn has_column(conn: &dyn DbConnection, table: &str, column: &str) -> bool {
        conn.get_table_columns(table).unwrap().contains(column)
    }

    /// Regression: dropping a locale from the config left its junction rows
    /// stored and unreachable — the scan only ever looked at columns, so
    /// `--confirm` cleaned the schema and left the rows.
    #[test]
    fn detects_junction_rows_of_a_locale_no_longer_configured() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _locale TEXT, label TEXT);
             INSERT INTO posts_items VALUES ('a', 'p1', 'en', 'kept');
             INSERT INTO posts_items VALUES ('b', 'p1', 'fr', 'stale');
             INSERT INTO posts_items VALUES ('c', 'p2', 'fr', 'stale');",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection("posts", vec![localized_array("items")])),
        );

        let stale = find_stale_locale_rows(&conn, &reg, &locale_en_de()).unwrap();
        assert_eq!(stale, vec![("posts_items".to_string(), 2)]);

        let deleted = delete_stale_locale_rows(&conn, &stale, &locale_en_de()).unwrap();
        assert_eq!(deleted, vec![("posts_items".to_string(), 2)]);

        assert!(
            find_stale_locale_rows(&conn, &reg, &locale_en_de())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            conn.query_all("SELECT id FROM posts_items", &[])
                .unwrap()
                .len(),
            1,
            "only the configured locale's row survives"
        );
    }

    /// Regression: deleting stale-locale junction rows left the references
    /// they held counted, outside any transaction — the target stayed
    /// undeletable for good. Applying the cleanup now recounts in the same
    /// transaction.
    #[cfg(feature = "sqlite")]
    #[test]
    fn applying_the_cleanup_recounts_the_deleted_rows_references() {
        let dir = TempDir::new().unwrap();
        let mut cfg = CrapConfig::test_default();
        cfg.database.path = "test.db".into();
        let db_pool = pool::create_pool(dir.path(), &cfg).unwrap();

        let localized_tags = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .localized(true)
            .build();

        let mut reg = Registry::default();
        reg.collections.insert(
            "tags".into(),
            Arc::new(simple_collection("tags", Vec::new())),
        );
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection("posts", vec![localized_tags])),
        );
        migrate::sync_all(&db_pool, &reg, &locale_en_de()).unwrap();

        let mut conn = db_pool.get().unwrap();
        conn.execute_batch(
            "INSERT INTO tags (id) VALUES ('t1');
             INSERT INTO tags (id) VALUES ('t2');
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 't1', 0, 'en');
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 't2', 0, 'fr');",
        )
        .unwrap();
        migrate::recompute_ref_counts(&conn, &reg, &locale_en_de()).unwrap();
        assert_eq!(
            ref_count_of(&conn, "tags", "t2"),
            1,
            "precondition: the stale row's reference is counted"
        );

        let mut report = scan(&conn, &reg, &locale_en_de()).unwrap();
        assert_eq!(
            report.stale_locale_rows,
            vec![("posts_tags".to_string(), 1)]
        );
        report.tables.clear();

        let outcome = apply_in_transaction(&mut conn, &report, &reg, &locale_en_de()).unwrap();
        assert_eq!(outcome.deleted_rows, vec![("posts_tags".to_string(), 1)]);

        assert_eq!(
            ref_count_of(&conn, "tags", "t2"),
            0,
            "the deleted row's reference must stop counting"
        );
        assert_eq!(
            ref_count_of(&conn, "tags", "t1"),
            1,
            "the kept row still counts"
        );
    }

    /// Regression: each "Dropped:" line was printed inside the transaction, so
    /// a later failure rolled the drop back while the screen still claimed it
    /// happened. A failing step now returns no outcome at all, and the column
    /// dropped before the failure is still there.
    #[cfg(feature = "sqlite")]
    #[test]
    fn a_failed_cleanup_rolls_back_and_reports_nothing() {
        let (_dir, mut conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, old_field TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection("posts", vec![text_field("title")])),
        );

        let report = CleanupReport {
            columns: vec![(
                "posts".to_string(),
                vec!["old_field".to_string(), "never_existed".to_string()],
            )],
            stale_locale_rows: Vec::new(),
            tables: Vec::new(),
        };

        let result = apply_in_transaction(&mut conn, &report, &reg, &locale_en_de());

        assert!(result.is_err(), "dropping a missing column must fail");
        assert!(
            has_column(&conn, "posts", "old_field"),
            "the column dropped before the failure is restored by the rollback"
        );
    }

    /// A committed cleanup reports every column it dropped.
    #[cfg(feature = "sqlite")]
    #[test]
    fn a_committed_cleanup_reports_the_dropped_columns() {
        let dir = TempDir::new().unwrap();
        let mut cfg = CrapConfig::test_default();
        cfg.database.path = "test.db".into();
        let db_pool = pool::create_pool(dir.path(), &cfg).unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection("posts", vec![text_field("title")])),
        );
        migrate::sync_all(&db_pool, &reg, &locale_en_de()).unwrap();

        let mut conn = db_pool.get().unwrap();
        conn.execute_batch("ALTER TABLE posts ADD COLUMN old_field TEXT")
            .unwrap();

        let mut report = scan(&conn, &reg, &locale_en_de()).unwrap();
        report.tables.clear();

        let outcome = apply_in_transaction(&mut conn, &report, &reg, &locale_en_de()).unwrap();

        assert_eq!(outcome.dropped_columns, vec!["posts.old_field".to_string()]);
        assert!(outcome.deleted_rows.is_empty());
        assert!(outcome.dropped_tables.is_empty());
        assert!(!has_column(&conn, "posts", "old_field"));
    }
}
