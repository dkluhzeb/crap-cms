//! `status` command — show project status (collections, globals, migrations, jobs, uploads).

use anyhow::{Context as _, Result, anyhow};
use std::{fs, path::Path};

use crate::{
    cli::{self, Table},
    config::CrapConfig,
    core::Registry,
    db::{DbConnection, DbPool, migrate, pool, query},
    hooks,
};

/// Format a byte count as a human-readable string (e.g., "1.5 MB").
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// Recursively sum file sizes in a directory.
fn dir_size(path: &Path) -> u64 {
    if !path.is_dir() {
        return 0;
    }
    let mut total = 0u64;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata();

            if let Ok(m) = meta {
                if m.is_dir() {
                    total += dir_size(&entry.path());
                } else {
                    total += m.len();
                }
            }
        }
    }
    total
}

/// Print database info (path and size for SQLite, backend name otherwise).
fn print_db_info(cfg: &CrapConfig, config_dir: &Path, conn: &dyn DbConnection) {
    match conn.kind() {
        "sqlite" => {
            let db_path = cfg.db_path(config_dir);
            let db_size = fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

            cli::kv(
                "Database",
                &format!("{} ({})", db_path.display(), format_bytes(db_size)),
            );
        }
        other => {
            cli::kv("Database", &format!("{} backend", other));
        }
    }
}

/// Print upload directory stats (total size and file count).
fn print_uploads_info(config_dir: &Path) {
    let uploads_dir = config_dir.join("uploads");

    if uploads_dir.is_dir() {
        let uploads_size = dir_size(&uploads_dir);
        let file_count = walkdir_count(&uploads_dir);

        cli::kv(
            "Uploads",
            &format!("{} ({} file(s))", format_bytes(uploads_size), file_count),
        );
    }
}

/// Print locale configuration summary.
fn print_locale_info(cfg: &CrapConfig) {
    if cfg.locale.is_enabled() {
        cli::kv(
            "Locales",
            &format!(
                "{} (default: {}{})",
                cfg.locale.locales.join(", "),
                cfg.locale.default_locale,
                if cfg.locale.fallback {
                    ", fallback enabled"
                } else {
                    ""
                }
            ),
        );
    }
}

/// Print collections table with row counts and tags.
fn print_collections(reg: &Registry, conn: &dyn DbConnection) {
    if reg.collections.is_empty() {
        cli::dim("Collections: (none)");
        return;
    }

    let mut table = Table::new(vec!["Collection", "Rows", "Tags"]);
    let mut slugs: Vec<_> = reg.collections.keys().collect();

    slugs.sort();

    for slug in slugs {
        let def = &reg.collections[slug];
        let count = query::count(conn, slug, def, &[], None).unwrap_or(0);
        let mut tags = Vec::new();

        if def.is_auth_collection() {
            tags.push("auth");
        }

        if def.is_upload_collection() {
            tags.push("upload");
        }

        if def.has_versions() {
            tags.push("versions");
        }

        let tag_str = tags.join(", ");

        table.row(vec![slug, &count.to_string(), &tag_str]);
    }

    table.print();
}

/// Print globals table.
fn print_globals(reg: &Registry) {
    if reg.globals.is_empty() {
        cli::dim("Globals: (none)");
        return;
    }

    let mut table = Table::new(vec!["Global"]);
    let mut slugs: Vec<_> = reg.globals.keys().collect();

    slugs.sort();

    for slug in slugs {
        table.row(vec![slug]);
    }

    table.print();
}

/// Print migration status (total, applied, pending).
fn print_migrations(config_dir: &Path, pool: &DbPool) {
    let migrations_dir = config_dir.join("migrations");
    let all_files = migrate::list_migration_files(&migrations_dir).unwrap_or_default();
    let applied = migrate::get_applied_migrations(pool).unwrap_or_default();
    let pending = all_files.iter().filter(|f| !applied.contains(*f)).count();

    cli::kv(
        "Migrations",
        &format!(
            "{} total, {} applied, {} pending",
            all_files.len(),
            applied.len(),
            pending
        ),
    );
}

/// Print jobs summary (defined, running, failed in last 24h).
fn print_jobs(reg: &Registry, conn: &dyn DbConnection, config_dir: &Path) {
    let jobs_dir = config_dir.join("jobs");

    if !jobs_dir.is_dir() {
        return;
    }

    let defined = reg.jobs.len();
    let running: i64 = conn
        .query_one(
            "SELECT COUNT(*) AS cnt FROM _crap_jobs WHERE status = 'running'",
            &[],
        )
        .ok()
        .flatten()
        .and_then(|r| r.get_i64("cnt").ok())
        .unwrap_or(0);
    let failed_24h = query::jobs::count_failed_since(conn, 86400).unwrap_or(0);

    let mut parts = vec![format!("{} defined", defined)];

    if running > 0 {
        parts.push(format!("{} running", running));
    }

    if failed_24h > 0 {
        parts.push(format!("{} failed (24h)", failed_24h));
    }

    cli::kv("Jobs", &parts.join(", "));
}

/// Print project status: collections, globals, migrations, jobs, uploads, locale.
pub fn run(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let conn = pool.get().context("Failed to get database connection")?;

    cli::header("Project Status");
    cli::kv("Config", &config_dir.display().to_string());
    print_db_info(&cfg, &config_dir, &conn);
    print_uploads_info(&config_dir);
    print_locale_info(&cfg);

    println!();
    print_collections(&reg, &conn);

    println!();
    print_globals(&reg);

    println!();
    print_migrations(&config_dir, &pool);
    print_jobs(&reg, &conn, &config_dir);

    Ok(())
}

/// Count files (non-directories) in a directory recursively.
fn walkdir_count(path: &Path) -> usize {
    let mut count = 0;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata() {
                if m.is_dir() {
                    count += walkdir_count(&entry.path());
                } else {
                    count += 1;
                }
            }
        }
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_values() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1048576), "1.0 MB");
        assert_eq!(format_bytes(1073741824), "1.0 GB");
    }

    #[test]
    fn dir_size_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(tmp.path()), 0);
    }

    #[test]
    fn dir_size_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        fs::write(tmp.path().join("b.txt"), "world!").unwrap();
        assert_eq!(dir_size(tmp.path()), 11); // 5 + 6
    }

    #[test]
    fn dir_size_nested() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("a.txt"), "abc").unwrap();
        fs::write(tmp.path().join("sub/b.txt"), "defg").unwrap();
        assert_eq!(dir_size(tmp.path()), 7); // 3 + 4
    }

    #[test]
    fn dir_size_nonexistent() {
        assert_eq!(dir_size(Path::new("/nonexistent/path")), 0);
    }

    #[test]
    fn walkdir_count_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(walkdir_count(tmp.path()), 0);
    }

    #[test]
    fn walkdir_count_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("a.txt"), "").unwrap();
        fs::write(tmp.path().join("sub/b.txt"), "").unwrap();
        assert_eq!(walkdir_count(tmp.path()), 2);
    }
}
