//! `migrate`, `db console`, `backup`, and `db cleanup` commands.

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::{
    config::{CrapConfig, LocaleConfig},
    core::{FieldType, Registry},
    db::{migrate, pool},
    hooks,
    hooks::HookRunner,
    scaffold,
};

/// Handle the `migrate` subcommand.
/// Untestable as unit: requires full Lua VM + DB setup. Covered by CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn migrate(config_dir: &Path, action: super::MigrateAction) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    // Create only writes a file — no Lua/DB needed
    if let super::MigrateAction::Create { ref name } = action {
        return scaffold::make_migration(&config_dir, name);
    }

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    match action {
        super::MigrateAction::Create { .. } => unreachable!(),
        super::MigrateAction::Up => {
            // Schema sync from Lua definitions
            println!("Syncing schema from Lua definitions...");
            migrate::sync_all(&pool, &registry, &cfg.locale)
                .context("Failed to sync database schema")?;
            println!("Schema sync complete.");

            // Run pending Lua data migrations
            let migrations_dir = config_dir.join("migrations");
            let pending = migrate::get_pending_migrations(&pool, &migrations_dir)?;

            if pending.is_empty() {
                println!("No pending migrations.");
            } else {
                let hook_runner = HookRunner::builder()
                    .config_dir(&config_dir)
                    .registry(registry.clone())
                    .config(&cfg)
                    .build()?;
                for filename in &pending {
                    let path = migrations_dir.join(filename);
                    let mut conn = pool.get().context("Failed to get DB connection")?;
                    let tx = conn.transaction().context("Failed to begin transaction")?;
                    hook_runner.run_migration(&path, "up", &tx)?;
                    migrate::record_migration(&tx, filename)?;
                    tx.commit()
                        .with_context(|| format!("Failed to commit migration {}", filename))?;
                    println!("Applied: {}", filename);
                }
                println!("{} migration(s) applied.", pending.len());
            }
        }
        super::MigrateAction::Down { steps } => {
            let applied = migrate::get_applied_migrations_desc(&pool)?;
            let to_rollback: Vec<_> = applied.into_iter().take(steps).collect();

            if to_rollback.is_empty() {
                println!("No migrations to roll back.");
            } else {
                let hook_runner = HookRunner::builder()
                    .config_dir(&config_dir)
                    .registry(registry.clone())
                    .config(&cfg)
                    .build()?;
                let migrations_dir = config_dir.join("migrations");
                for filename in &to_rollback {
                    let path = migrations_dir.join(filename);

                    if !path.exists() {
                        bail!("Migration file not found: {}", path.display());
                    }
                    let mut conn = pool.get().context("Failed to get DB connection")?;
                    let tx = conn.transaction().context("Failed to begin transaction")?;
                    hook_runner.run_migration(&path, "down", &tx)?;
                    migrate::remove_migration(&tx, filename)?;
                    tx.commit()
                        .with_context(|| format!("Failed to commit rollback of {}", filename))?;
                    println!("Rolled back: {}", filename);
                }
                println!("{} migration(s) rolled back.", to_rollback.len());
            }
        }
        super::MigrateAction::List => {
            let migrations_dir = config_dir.join("migrations");
            let all_files = migrate::list_migration_files(&migrations_dir)?;
            let applied = migrate::get_applied_migrations(&pool)?;

            if all_files.is_empty() {
                println!("No migration files found in {}", migrations_dir.display());
            } else {
                println!("{:<50} Status", "Migration");
                println!("{}", "-".repeat(60));
                for f in &all_files {
                    let status = if applied.contains(f) {
                        "applied"
                    } else {
                        "pending"
                    };
                    println!("{:<50} {}", f, status);
                }
            }
        }
        super::MigrateAction::Fresh { confirm } => {
            if !confirm {
                bail!(
                    "migrate fresh is destructive — it drops ALL tables and recreates them.\n\
                     Pass --confirm to proceed."
                );
            }

            println!("Dropping all tables...");
            migrate::drop_all_tables(&pool)?;
            println!("Tables dropped.");

            println!("Recreating schema from Lua definitions...");
            migrate::sync_all(&pool, &registry, &cfg.locale)
                .context("Failed to sync database schema")?;
            println!("Schema sync complete.");

            // Run all migrations from scratch
            let migrations_dir = config_dir.join("migrations");
            let all_files = migrate::list_migration_files(&migrations_dir)?;

            if !all_files.is_empty() {
                let hook_runner = HookRunner::builder()
                    .config_dir(&config_dir)
                    .registry(registry.clone())
                    .config(&cfg)
                    .build()?;
                for filename in &all_files {
                    let path = migrations_dir.join(filename);
                    let mut conn = pool.get().context("Failed to get DB connection")?;
                    let tx = conn.transaction().context("Failed to begin transaction")?;
                    hook_runner.run_migration(&path, "up", &tx)?;
                    migrate::record_migration(&tx, filename)?;
                    tx.commit()
                        .with_context(|| format!("Failed to commit migration {}", filename))?;
                    println!("Applied: {}", filename);
                }
                println!("{} migration(s) applied.", all_files.len());
            }

            println!("Fresh migration complete.");
        }
    }

    Ok(())
}

/// Open an interactive SQLite console.
/// Untestable: spawns interactive sqlite3 process.
#[cfg(not(tarpaulin_include))]
pub fn console(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;

    let db_path = cfg.db_path(&config_dir);

    if !db_path.exists() {
        bail!("Database file not found: {}", db_path.display());
    }

    println!("Opening SQLite console: {}", db_path.display());

    let status = std::process::Command::new("sqlite3")
        .arg(&db_path)
        .status()
        .context("Failed to launch sqlite3 — is it installed?")?;

    if !status.success() {
        bail!("sqlite3 exited with status {}", status);
    }

    Ok(())
}

/// Handle the `backup` subcommand.
/// Untestable: spawns tar process for uploads, opens raw SQLite connection, writes timestamped files.
/// Covered by CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn backup(config_dir: &Path, output: Option<PathBuf>, include_uploads: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;

    let db_path = cfg.db_path(&config_dir);

    if !db_path.exists() {
        bail!("Database file not found: {}", db_path.display());
    }

    // Determine backup directory
    let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let backup_dir_name = format!("backup-{}", timestamp);
    let backup_base = output.unwrap_or_else(|| config_dir.join("backups"));
    let backup_dir = backup_base.join(&backup_dir_name);

    std::fs::create_dir_all(&backup_dir).with_context(|| {
        format!(
            "Failed to create backup directory: {}",
            backup_dir.display()
        )
    })?;

    // VACUUM INTO for a consistent snapshot
    let backup_db_path = backup_dir.join("crap.db");
    println!("Creating database snapshot...");
    {
        let conn =
            rusqlite::Connection::open(&db_path).context("Failed to open database for backup")?;
        conn.execute(
            "VACUUM INTO ?1",
            [backup_db_path.to_string_lossy().as_ref()],
        )
        .context("VACUUM INTO failed")?;
    }
    let db_size = std::fs::metadata(&backup_db_path)
        .map(|m| m.len())
        .unwrap_or(0);
    println!(
        "Database snapshot: {} ({} bytes)",
        backup_db_path.display(),
        db_size
    );

    // Optionally backup uploads
    let mut uploads_size: Option<u64> = None;

    if include_uploads {
        let uploads_dir = config_dir.join("uploads");

        if uploads_dir.exists() && uploads_dir.is_dir() {
            let archive_path = backup_dir.join("uploads.tar.gz");
            println!("Compressing uploads...");
            let status = std::process::Command::new("tar")
                .args([
                    "czf",
                    &archive_path.to_string_lossy(),
                    "-C",
                    &config_dir.to_string_lossy(),
                    "uploads",
                ])
                .status();
            match status {
                Ok(s) if s.success() => {
                    uploads_size = std::fs::metadata(&archive_path).map(|m| m.len()).ok();
                    println!(
                        "Uploads archive: {} ({} bytes)",
                        archive_path.display(),
                        uploads_size.unwrap_or(0)
                    );
                }
                Ok(s) => {
                    eprintln!("Warning: tar exited with status {}", s);
                }
                Err(e) => {
                    eprintln!(
                        "Warning: tar not found or failed: {}. Skipping uploads backup.",
                        e
                    );
                }
            }
        } else {
            println!("No uploads directory found — skipping.");
        }
    }

    // Write manifest.json
    let manifest = json!({
        "crap_version": env!("CARGO_PKG_VERSION"),
        "timestamp": chrono::Local::now().to_rfc3339(),
        "db_size": db_size,
        "uploads_size": uploads_size,
        "include_uploads": include_uploads,
        "source_db": db_path.to_string_lossy(),
        "source_config": config_dir.to_string_lossy(),
    });
    let manifest_path = backup_dir.join("manifest.json");
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)
        .context("Failed to write manifest.json")?;

    println!("\nBackup complete: {}", backup_dir.display());
    Ok(())
}

/// Handle the `restore` subcommand.
/// Untestable: replaces database file and spawns tar process.
/// Covered by CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn restore(
    config_dir: &Path,
    backup_dir: &Path,
    include_uploads: bool,
    confirm: bool,
) -> Result<()> {
    if !confirm {
        bail!(
            "Restore is destructive — it replaces the current database.\n\
             Pass --confirm / -y to proceed."
        );
    }

    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let backup_dir = backup_dir
        .canonicalize()
        .unwrap_or_else(|_| backup_dir.to_path_buf());

    // Validate backup directory
    let manifest_path = backup_dir.join("manifest.json");
    let backup_db_path = backup_dir.join("crap.db");

    if !manifest_path.exists() {
        bail!("No manifest.json found in {}", backup_dir.display());
    }
    if !backup_db_path.exists() {
        bail!("No crap.db found in {}", backup_dir.display());
    }

    // Read and display manifest
    let manifest_str =
        std::fs::read_to_string(&manifest_path).context("Failed to read manifest.json")?;
    let manifest: Value =
        serde_json::from_str(&manifest_str).context("Failed to parse manifest.json")?;

    println!("Restoring from backup:");
    println!(
        "  Version:   {}",
        manifest
            .get("crap_version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    );
    println!(
        "  Timestamp: {}",
        manifest
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    );
    println!(
        "  DB size:   {} bytes",
        manifest
            .get("db_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );

    if let Some(size) = manifest.get("uploads_size").and_then(|v| v.as_u64()) {
        println!("  Uploads:   {} bytes", size);
    }

    // Load config to find target DB path
    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let db_path = cfg.db_path(&config_dir);

    // Ensure target directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    // Replace database file
    println!("Restoring database to {}...", db_path.display());
    std::fs::copy(&backup_db_path, &db_path)
        .with_context(|| format!("Failed to copy database to {}", db_path.display()))?;
    println!("Database restored.");

    // Remove WAL/SHM files if they exist (stale from previous instance)
    let wal_path = db_path.with_extension("db-wal");
    let shm_path = db_path.with_extension("db-shm");

    if wal_path.exists() {
        let _ = std::fs::remove_file(&wal_path);
    }
    if shm_path.exists() {
        let _ = std::fs::remove_file(&shm_path);
    }

    // Optionally restore uploads
    if include_uploads {
        let archive_path = backup_dir.join("uploads.tar.gz");

        if archive_path.exists() {
            println!("Extracting uploads...");
            let status = std::process::Command::new("tar")
                .args([
                    "xzf",
                    &archive_path.to_string_lossy(),
                    "-C",
                    &config_dir.to_string_lossy(),
                ])
                .status();
            match status {
                Ok(s) if s.success() => {
                    println!("Uploads restored.");
                }
                Ok(s) => {
                    eprintln!("Warning: tar exited with status {}", s);
                }
                Err(e) => {
                    eprintln!(
                        "Warning: tar not found or failed: {}. Skipping uploads restore.",
                        e
                    );
                }
            }
        } else {
            println!("No uploads.tar.gz in backup — skipping uploads restore.");
        }
    }

    println!("\nRestore complete.");
    Ok(())
}

/// Detect and optionally remove orphan columns not present in Lua definitions.
///
/// Orphan columns are columns that exist in a collection table but do not correspond
/// to any field in the current Lua definition. System columns (`_`-prefixed) are
/// always kept. Because Lua definitions include plugin-added fields (plugins run
/// during `init_lua`), plugin columns are never flagged as orphans.
///
/// By default runs in dry-run mode (report only). Pass `confirm = true` to actually
/// drop orphan columns.
#[cfg(not(tarpaulin_include))]
pub fn cleanup(config_dir: &Path, confirm: bool) -> Result<()> {
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

    let orphans = find_orphan_columns(&conn, &reg, &cfg.locale)?;

    if orphans.is_empty() {
        println!("No orphan columns found. All columns match Lua definitions.");

        return Ok(());
    }

    println!("Orphan columns (not in Lua definitions):\n");
    for (table, cols) in &orphans {
        for col in cols {
            println!("  {}.{}", table, col);
        }
    }

    let total: usize = orphans.iter().map(|(_, cols)| cols.len()).sum();
    println!("\n{} orphan column(s) found.", total);

    if !confirm {
        println!("\nThis is a dry run. Pass --confirm to drop these columns.");
        println!("Note: dropping columns is irreversible. Back up your database first.");

        return Ok(());
    }

    // SQLite supports DROP COLUMN since 3.35.0 (2021-03-12).
    // Check version first.
    let version: String = conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let parts: Vec<u32> = version.split('.').filter_map(|s| s.parse().ok()).collect();

    if parts.len() >= 2 && (parts[0] < 3 || (parts[0] == 3 && parts[1] < 35)) {
        bail!(
            "SQLite {} does not support DROP COLUMN (requires 3.35.0+). \
             Consider recreating the table manually.",
            version
        );
    }

    for (table, cols) in &orphans {
        for col in cols {
            let sql = format!("ALTER TABLE {} DROP COLUMN {}", table, col);
            conn.execute(&sql, [])
                .with_context(|| format!("Failed to drop column {}.{}", table, col))?;
            println!("Dropped: {}.{}", table, col);
        }
    }

    println!("\n{} column(s) dropped.", total);
    Ok(())
}

/// Find orphan columns across all collection tables.
///
/// Returns a vec of (table_name, vec_of_orphan_column_names).
/// System columns (`_`-prefixed, `id`, `created_at`, `updated_at`) are excluded.
/// Plugin columns are NOT orphans because plugins run during `init_lua` and their
/// fields are included in the registry definitions.
pub fn find_orphan_columns(
    conn: &rusqlite::Connection,
    reg: &Registry,
    locale_config: &LocaleConfig,
) -> Result<Vec<(String, Vec<String>)>> {
    let mut results = Vec::new();

    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        let def = &reg.collections[slug];

        // Get actual DB columns
        let existing = migrate::helpers::get_table_columns(conn, slug)?;

        if existing.is_empty() {
            continue; // table doesn't exist yet
        }

        // Build expected column names from Lua definition
        let mut expected: HashSet<String> = HashSet::new();
        expected.insert("id".to_string());

        for field in &def.fields {
            if field.field_type == FieldType::Group {
                for sub in &field.fields {
                    let base = format!("{}__{}", field.name, sub.name);
                    let is_localized =
                        (field.localized || sub.localized) && locale_config.is_enabled();

                    if is_localized {
                        for locale in &locale_config.locales {
                            expected.insert(format!("{}__{}", base, locale));
                        }
                    } else {
                        expected.insert(base);
                    }
                }
                continue;
            }
            if field.field_type == FieldType::Row || field.field_type == FieldType::Collapsible {
                for sub in &field.fields {
                    let is_localized = sub.localized && locale_config.is_enabled();

                    if is_localized {
                        for locale in &locale_config.locales {
                            expected.insert(format!("{}__{}", sub.name, locale));
                        }
                    } else {
                        expected.insert(sub.name.clone());
                    }
                }
                continue;
            }
            if field.field_type == FieldType::Tabs {
                for tab in &field.tabs {
                    for sub in &tab.fields {
                        let is_localized = sub.localized && locale_config.is_enabled();

                        if is_localized {
                            for locale in &locale_config.locales {
                                expected.insert(format!("{}__{}", sub.name, locale));
                            }
                        } else {
                            expected.insert(sub.name.clone());
                        }
                    }
                }
                continue;
            }
            if !field.has_parent_column() {
                continue;
            }
            if field.localized && locale_config.is_enabled() {
                for locale in &locale_config.locales {
                    expected.insert(format!("{}__{}", field.name, locale));
                }
            } else {
                expected.insert(field.name.clone());
            }
        }

        if def.timestamps {
            expected.insert("created_at".to_string());
            expected.insert("updated_at".to_string());
        }

        // Find orphans: columns in DB but not in expected, excluding system columns
        let mut orphan_cols: Vec<String> = existing
            .iter()
            .filter(|col| {
                !expected.contains(*col) && !col.starts_with('_') // system columns: _password_hash, _locked, etc.
            })
            .cloned()
            .collect();

        if !orphan_cols.is_empty() {
            orphan_cols.sort();
            results.push((slug.to_string(), orphan_cols));
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::Registry;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType};

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

    fn simple_collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.timestamps = true;
        def.fields = fields;
        def
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn make_conn() -> rusqlite::Connection {
        rusqlite::Connection::open_in_memory().unwrap()
    }

    #[test]
    fn no_orphans_when_columns_match() {
        let conn = make_conn();
        conn.execute(
            "CREATE TABLE posts (id TEXT, title TEXT, created_at TEXT, updated_at TEXT)",
            [],
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection("posts", vec![text_field("title")]),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn detects_orphan_column() {
        let conn = make_conn();
        conn.execute("CREATE TABLE posts (id TEXT, title TEXT, old_field TEXT, created_at TEXT, updated_at TEXT)", []).unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection("posts", vec![text_field("title")]),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].0, "posts");
        assert_eq!(orphans[0].1, vec!["old_field"]);
    }

    #[test]
    fn system_columns_not_orphans() {
        let conn = make_conn();
        conn.execute(
            "CREATE TABLE users (id TEXT, email TEXT, _password_hash TEXT, _locked INTEGER, created_at TEXT, updated_at TEXT)",
            [],
        ).unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "users".into(),
            simple_collection("users", vec![text_field("email")]),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty(), "system columns should not be flagged");
    }

    #[test]
    fn group_fields_not_orphans() {
        let conn = make_conn();
        conn.execute(
            "CREATE TABLE posts (id TEXT, seo__meta_title TEXT, seo__meta_desc TEXT, created_at TEXT, updated_at TEXT)",
            [],
        ).unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![text_field("meta_title"), text_field("meta_desc")])
                        .build(),
                ],
            ),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty(), "group fields should not be flagged");
    }

    #[test]
    fn localized_columns_not_orphans() {
        let conn = make_conn();
        conn.execute(
            "CREATE TABLE posts (id TEXT, title__en TEXT, title__de TEXT, created_at TEXT, updated_at TEXT)",
            [],
        ).unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .localized(true)
                        .build(),
                ],
            ),
        );

        let orphans = find_orphan_columns(&conn, &reg, &locale_en_de()).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn detects_orphan_among_valid_columns() {
        let conn = make_conn();
        conn.execute(
            "CREATE TABLE posts (id TEXT, title TEXT, removed_field TEXT, seo__meta TEXT, created_at TEXT, updated_at TEXT)",
            [],
        ).unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            simple_collection(
                "posts",
                vec![
                    text_field("title"),
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![text_field("meta")])
                        .build(),
                ],
            ),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].1, vec!["removed_field"]);
    }
}
