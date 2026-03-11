//! Migration tracking: list, record, remove, and manage migration files.

use anyhow::{Context as _, Result};
use std::collections::HashSet;

use super::helpers::table_exists;
use crate::db::DbPool;

/// List all `*.lua` files in the migrations directory, sorted by filename (chronological).
pub fn list_migration_files(migrations_dir: &std::path::Path) -> Result<Vec<String>> {
    if !migrations_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(migrations_dir)
        .with_context(|| format!("Failed to read {}", migrations_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua") {
            if let Some(name) = path.file_name() {
                files.push(name.to_string_lossy().to_string());
            }
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
    let mut stmt = conn.prepare("SELECT filename FROM _crap_migrations")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
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
    let mut stmt = conn
        .prepare("SELECT filename FROM _crap_migrations ORDER BY applied_at DESC, filename DESC")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut list = Vec::new();
    for r in rows {
        list.push(r?);
    }
    Ok(list)
}

/// Get pending migration filenames (files on disk minus already applied), sorted ascending.
pub fn get_pending_migrations(
    pool: &DbPool,
    migrations_dir: &std::path::Path,
) -> Result<Vec<String>> {
    let all = list_migration_files(migrations_dir)?;
    let applied = get_applied_migrations(pool)?;
    Ok(all.into_iter().filter(|f| !applied.contains(f)).collect())
}

/// Record a migration as applied.
pub fn record_migration(conn: &rusqlite::Connection, filename: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO _crap_migrations (filename) VALUES (?1)",
        [filename],
    )
    .with_context(|| format!("Failed to record migration {}", filename))?;
    Ok(())
}

/// Remove a migration record (for rollback).
pub fn remove_migration(conn: &rusqlite::Connection, filename: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM _crap_migrations WHERE filename = ?1",
        [filename],
    )
    .with_context(|| format!("Failed to remove migration record {}", filename))?;
    Ok(())
}

/// Drop all user tables (for `migrate fresh`). Drops everything except sqlite internals.
pub fn drop_all_tables(pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    for table in &tables {
        conn.execute(&format!("DROP TABLE IF EXISTS \"{}\"", table), [])
            .with_context(|| format!("Failed to drop table {}", table))?;
        tracing::info!("Dropped table: {}", table);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;

    fn in_memory_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory().with_flags(
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_SHARED_CACHE,
        );
        r2d2::Pool::builder()
            .max_size(2)
            .build(manager)
            .expect("in-memory pool")
    }

    // ── migration tracking ────────────────────────────────────────────────

    #[test]
    fn migration_tracking_roundtrip() {
        let pool = in_memory_pool();
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
        let pool = in_memory_pool();
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
        let pool = in_memory_pool();
        let applied = get_applied_migrations(&pool).unwrap();
        assert!(applied.is_empty());
    }

    #[test]
    fn get_pending_migrations_filters_applied() {
        let pool = in_memory_pool();
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
        let pool = in_memory_pool();
        {
            let conn = pool.get().unwrap();
            conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", [])
                .unwrap();
            conn.execute("CREATE TABLE users (id TEXT PRIMARY KEY)", [])
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
        let pool = in_memory_pool();
        let result = get_applied_migrations_desc(&pool).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn get_applied_migrations_desc_ordering() {
        let pool = in_memory_pool();
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
