//! Migration tracking: list, record, remove, and manage migration files.

use anyhow::{Context as _, Result};
use std::{collections::HashSet, fs, path::Path};

use crate::db::migrate::helpers::table_exists;
use crate::db::{DbConnection, DbPool, DbValue};

/// List all `*.lua` files in the migrations directory, sorted by filename (chronological).
pub fn list_migration_files(migrations_dir: &Path) -> Result<Vec<String>> {
    if !migrations_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(migrations_dir)
        .with_context(|| format!("Failed to read {}", migrations_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.extension().is_some_and(|ext| ext == "lua")
            && let Some(name) = path.file_name()
        {
            files.push(name.to_string_lossy().to_string());
        }
    }
    files.sort();
    Ok(files)
}

/// Get filenames of all applied migrations (unordered set).
pub fn get_applied_migrations(pool: &DbPool) -> Result<HashSet<String>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    // Table may not exist yet if sync_all hasn't run
    let exists = table_exists(&conn, "_crap_migrations")?;

    if !exists {
        return Ok(HashSet::new());
    }
    let rows = conn.query_all("SELECT filename FROM _crap_migrations", &[])?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r.get_string("filename")?);
    }
    Ok(set)
}

/// Get applied migration filenames, most recent first.
pub fn get_applied_migrations_desc(pool: &DbPool) -> Result<Vec<String>> {
    let conn = pool.get().context("Failed to get DB connection")?;
    let exists = table_exists(&conn, "_crap_migrations")?;

    if !exists {
        return Ok(Vec::new());
    }
    let rows = conn.query_all(
        "SELECT filename FROM _crap_migrations ORDER BY applied_at DESC, filename DESC",
        &[],
    )?;
    let mut list = Vec::new();
    for r in rows {
        list.push(r.get_string("filename")?);
    }
    Ok(list)
}

/// Get pending migration filenames (files on disk minus already applied), sorted ascending.
pub fn get_pending_migrations(pool: &DbPool, migrations_dir: &Path) -> Result<Vec<String>> {
    let all = list_migration_files(migrations_dir)?;
    let applied = get_applied_migrations(pool)?;
    Ok(all.into_iter().filter(|f| !applied.contains(f)).collect())
}

/// Record a migration as applied.
pub fn record_migration(conn: &dyn DbConnection, filename: &str) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO _crap_migrations (filename) VALUES ({})",
            conn.placeholder(1)
        ),
        &[DbValue::Text(filename.to_string())],
    )
    .with_context(|| format!("Failed to record migration {}", filename))?;
    Ok(())
}

/// Remove a migration record (for rollback).
pub fn remove_migration(conn: &dyn DbConnection, filename: &str) -> Result<()> {
    conn.execute(
        &format!(
            "DELETE FROM _crap_migrations WHERE filename = {}",
            conn.placeholder(1)
        ),
        &[DbValue::Text(filename.to_string())],
    )
    .with_context(|| format!("Failed to remove migration record {}", filename))?;
    Ok(())
}

/// Drop all user tables (for `migrate fresh`). Drops everything except sqlite internals.
pub fn drop_all_tables(pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;
    let tables = conn.list_user_tables()?;

    for table in &tables {
        conn.execute(&format!("DROP TABLE IF EXISTS \"{}\"", table), &[])
            .with_context(|| format!("Failed to drop table {}", table))?;
        tracing::info!("Dropped table: {}", table);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{DbConnection, DbPool, pool};
    use tempfile::TempDir;

    fn in_memory_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("temp dir");
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).expect("in-memory pool");
        (dir, p)
    }

    // ── migration tracking ────────────────────────────────────────────────

    #[test]
    fn migration_tracking_roundtrip() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();

        record_migration(&conn, "001_init.lua").unwrap();
        record_migration(&conn, "002_add_field.lua").unwrap();

        let applied = get_applied_migrations(&pool).unwrap();
        assert!(applied.contains("001_init.lua"));
        assert!(applied.contains("002_add_field.lua"));
        assert_eq!(applied.len(), 2);
    }

    #[test]
    fn remove_migration_works() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();

        record_migration(&conn, "001_init.lua").unwrap();
        remove_migration(&conn, "001_init.lua").unwrap();

        let applied = get_applied_migrations(&pool).unwrap();
        assert!(applied.is_empty());
    }

    #[test]
    fn get_applied_migrations_no_table() {
        let (_dir, pool) = in_memory_pool();
        let applied = get_applied_migrations(&pool).unwrap();
        assert!(applied.is_empty());
    }

    #[test]
    fn get_pending_migrations_filters_applied() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();
        record_migration(&conn, "001_init.lua").unwrap();

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("001_init.lua"), "-- already applied").unwrap();
        std::fs::write(tmp.path().join("002_new.lua"), "-- pending").unwrap();

        let pending = get_pending_migrations(&pool, tmp.path()).unwrap();
        assert_eq!(pending, vec!["002_new.lua"]);
    }

    #[test]
    fn list_migration_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("003_z.lua"), "").unwrap();
        std::fs::write(tmp.path().join("001_a.lua"), "").unwrap();
        std::fs::write(tmp.path().join("002_b.lua"), "").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "").unwrap(); // non-lua

        let files = list_migration_files(tmp.path()).unwrap();
        assert_eq!(files, vec!["001_a.lua", "002_b.lua", "003_z.lua"]);
    }

    #[test]
    fn list_migration_files_missing_dir() {
        let files = list_migration_files(std::path::Path::new("/nonexistent/dir")).unwrap();
        assert!(files.is_empty());
    }

    // ── drop_all_tables ───────────────────────────────────────────────────

    #[test]
    fn drop_all_tables_cleans_everything() {
        let (_dir, pool) = in_memory_pool();
        {
            let conn = pool.get().unwrap();
            conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
                .unwrap();
            conn.execute("CREATE TABLE users (id TEXT PRIMARY KEY)", &[])
                .unwrap();
        }
        drop_all_tables(&pool).unwrap();
        let conn = pool.get().unwrap();
        assert!(!table_exists(&conn, "posts").unwrap());
        assert!(!table_exists(&conn, "users").unwrap());
    }

    // ── get_applied_migrations_desc ──────────────────────────────────────

    #[test]
    fn get_applied_migrations_desc_no_table() {
        let (_dir, pool) = in_memory_pool();
        let result = get_applied_migrations_desc(&pool).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn get_applied_migrations_desc_ordering() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_migrations (filename TEXT PRIMARY KEY, applied_at TEXT DEFAULT (datetime('now')))"
        ).unwrap();
        record_migration(&conn, "001_a.lua").unwrap();
        record_migration(&conn, "002_b.lua").unwrap();
        record_migration(&conn, "003_c.lua").unwrap();

        let applied = get_applied_migrations_desc(&pool).unwrap();
        assert_eq!(applied, vec!["003_c.lua", "002_b.lua", "001_a.lua"]);
    }
}
