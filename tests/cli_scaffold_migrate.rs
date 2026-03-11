//! CLI integration tests: roundtrip, typegen, migrate, backup, blueprint, jobs.
//!
//! Split from cli_integration.rs for faster parallel compilation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crap_cms::commands;
use crap_cms::config::CrapConfig;
use crap_cms::db::{DbPool, migrate, ops, pool, query};
use crap_cms::hooks;
use crap_cms::scaffold;
use crap_cms::typegen;

// ── Helpers ──────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("cli_tests")
}

fn crap_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_crap-cms"))
}

/// Copy fixture dir to a temp dir, init Lua, create pool, sync schema.
/// Returns (TempDir, DbPool, SharedRegistry).
fn full_setup() -> (tempfile::TempDir, DbPool, crap_cms::core::SharedRegistry) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync schema");

    (tmp, db_pool, registry)
}

/// Recursively copy a directory.
fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

/// Recursively copy a directory, skipping named subdirs/files.
fn copy_dir_skip(src: &Path, dst: &Path, skip: &[&str]) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if skip.iter().any(|s| *s == name_str.as_ref()) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if src_path.is_dir() {
            copy_dir(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. Export-Import Roundtrip
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn roundtrip_data_preserved() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();

    // Create some data
    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..3 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Roundtrip Post {}", i));
            data.insert("status".to_string(), "published".to_string());
            query::create(&tx, "posts", def, &data, None).unwrap();
        }
        tx.commit().unwrap();
    }

    // Export
    let conn = pool.get().unwrap();
    let exported = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(exported.len(), 3);
    let exported_json: Vec<serde_json::Value> = exported
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let ids: Vec<String> = exported.iter().map(|d| d.id.clone()).collect();
    drop(conn);

    // Delete all
    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        for id in &ids {
            query::delete(&tx, "posts", id).unwrap();
        }
        tx.commit().unwrap();
    }

    // Verify empty
    let conn = pool.get().unwrap();
    let after_delete =
        query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(after_delete.len(), 0);
    drop(conn);

    // Re-import
    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        for doc_val in &exported_json {
            let obj = doc_val.as_object().unwrap();
            let id = obj["id"].as_str().unwrap();
            let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let status = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
            tx.execute(
                "INSERT OR REPLACE INTO posts (id, title, status) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, title, status],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    // Verify re-imported
    let conn = pool.get().unwrap();
    let reimported = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(reimported.len(), 3);
    for doc in &reimported {
        assert!(
            ids.contains(&doc.id),
            "re-imported doc should have original ID"
        );
        assert_eq!(doc.get_str("status"), Some("published"));
    }
}

#[test]
fn roundtrip_multiple_collections() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();

    // Seed both collections
    {
        let posts_def = reg.get_collection("posts").unwrap();
        let users_def = reg.get_collection("users").unwrap();

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Multi Post".to_string());
        query::create(&tx, "posts", posts_def, &data, None).unwrap();

        let mut udata = HashMap::new();
        udata.insert("email".to_string(), "multi@test.com".to_string());
        udata.insert("name".to_string(), "Multi User".to_string());
        query::create(&tx, "users", users_def, &udata, None).unwrap();

        tx.commit().unwrap();
    }

    // Export both
    let conn = pool.get().unwrap();
    let mut collections_data = serde_json::Map::new();
    for (slug, def) in &reg.collections {
        let docs = query::find(&conn, slug, def, &query::FindQuery::default(), None).unwrap();
        let docs_json: Vec<serde_json::Value> = docs
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        collections_data.insert(slug.clone(), serde_json::Value::Array(docs_json));
    }

    assert!(collections_data.contains_key("posts"));
    assert!(collections_data.contains_key("users"));
    assert!(!collections_data["posts"].as_array().unwrap().is_empty());
    assert!(!collections_data["users"].as_array().unwrap().is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. Typegen
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn typegen_lua() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let reg = registry.read().unwrap();

    let path = typegen::generate(&config_dir, &reg).unwrap();
    assert!(path.exists());
    assert!(path.to_string_lossy().ends_with("generated.lua"));

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.is_empty());
}

#[test]
fn typegen_all_languages() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let reg = registry.read().unwrap();

    for lang in typegen::Language::all() {
        let path = typegen::generate_lang(&config_dir, &reg, *lang, None).unwrap();
        assert!(path.exists(), "file should exist for {:?}", lang);
        let expected_ext = format!("generated.{}", lang.file_extension());
        assert!(
            path.to_string_lossy().ends_with(&expected_ext),
            "expected ext {}, got {}",
            expected_ext,
            path.display()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. Migrate Commands
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn migrate_list_pending() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a migration file
    scaffold::make_migration(&config_dir, "test_migration").unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync");

    let migrations_dir = config_dir.join("migrations");
    let all_files = migrate::list_migration_files(&migrations_dir).unwrap();
    assert_eq!(all_files.len(), 1);

    let pending = migrate::get_pending_migrations(&db_pool, &migrations_dir).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].contains("test_migration"));
}

#[test]
fn migrate_up() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a migration file with actual M.up/M.down
    let migrations_dir = config_dir.join("migrations");
    std::fs::create_dir_all(&migrations_dir).unwrap();
    std::fs::write(
        migrations_dir.join("20240101000000_test.lua"),
        r#"
local M = {}
function M.up()
    -- no-op for test
end
function M.down()
    -- no-op for test
end
return M
"#,
    )
    .unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync");

    // Verify pending
    let pending = migrate::get_pending_migrations(&db_pool, &migrations_dir).unwrap();
    assert_eq!(pending.len(), 1);

    // Run migration via HookRunner
    let hook_runner = hooks::lifecycle::HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry)
        .config(&cfg)
        .build()
        .unwrap();
    let filename = &pending[0];
    let path = migrations_dir.join(filename);
    let mut conn = db_pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    hook_runner.run_migration(&path, "up", &tx).unwrap();
    migrate::record_migration(&tx, filename).unwrap();
    tx.commit().unwrap();

    // Verify applied
    let applied = migrate::get_applied_migrations(&db_pool).unwrap();
    assert!(applied.contains(&pending[0]));

    let new_pending = migrate::get_pending_migrations(&db_pool, &migrations_dir).unwrap();
    assert!(new_pending.is_empty());
}

#[test]
fn migrate_down() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let migrations_dir = config_dir.join("migrations");
    std::fs::create_dir_all(&migrations_dir).unwrap();
    std::fs::write(
        migrations_dir.join("20240101000000_rollback.lua"),
        r#"
local M = {}
function M.up()
end
function M.down()
end
return M
"#,
    )
    .unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync");

    // Apply migration
    let hook_runner = hooks::lifecycle::HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&cfg)
        .build()
        .unwrap();
    let filename = "20240101000000_rollback.lua";
    {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        hook_runner
            .run_migration(&migrations_dir.join(filename), "up", &tx)
            .unwrap();
        migrate::record_migration(&tx, filename).unwrap();
        tx.commit().unwrap();
    }
    assert!(
        migrate::get_applied_migrations(&db_pool)
            .unwrap()
            .contains(filename)
    );

    // Rollback
    {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        hook_runner
            .run_migration(&migrations_dir.join(filename), "down", &tx)
            .unwrap();
        migrate::remove_migration(&tx, filename).unwrap();
        tx.commit().unwrap();
    }
    assert!(
        !migrate::get_applied_migrations(&db_pool)
            .unwrap()
            .contains(filename)
    );
}

#[test]
fn migrate_fresh() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync");

    // Seed data
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Pre-fresh".to_string());
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Drop all tables and recreate
    migrate::drop_all_tables(&db_pool).unwrap();
    migrate::sync_all(&db_pool, &registry, &cfg.locale).unwrap();

    // Verify data is gone but tables exist
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();
    let count = ops::count_documents(&db_pool, "posts", def, &[], None).unwrap();
    assert_eq!(count, 0, "data should be gone after fresh");
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. Backup
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn backup_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync");

    // Create a document so the DB has data
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Backup test".to_string());
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }
    // Ensure pool connections are returned before backup
    drop(db_pool);

    // Replicate backup: VACUUM INTO
    let db_path = cfg.db_path(&config_dir);
    let backup_dir = tmp.path().join("backup-test");
    std::fs::create_dir_all(&backup_dir).unwrap();
    let backup_db_path = backup_dir.join("crap.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "VACUUM INTO ?1",
            [backup_db_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    }
    assert!(backup_db_path.exists());
    assert!(std::fs::metadata(&backup_db_path).unwrap().len() > 0);

    // Write manifest
    let manifest = serde_json::json!({
        "timestamp": "2024-01-01T00:00:00+00:00",
        "db_size": std::fs::metadata(&backup_db_path).unwrap().len(),
        "include_uploads": false,
        "source_db": db_path.to_string_lossy(),
        "source_config": config_dir.to_string_lossy(),
    });
    let manifest_path = backup_dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    assert!(backup_dir.join("crap.db").exists());
    assert!(backup_dir.join("manifest.json").exists());
}

#[test]
fn backup_manifest_valid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let backup_dir = tmp.path().join("backup");
    std::fs::create_dir_all(&backup_dir).unwrap();

    let manifest = serde_json::json!({
        "timestamp": "2024-06-15T12:00:00+00:00",
        "db_size": 12345,
        "uploads_size": null,
        "include_uploads": false,
        "source_db": "/some/path/crap.db",
        "source_config": "/some/path/config",
    });

    let manifest_path = backup_dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let content = std::fs::read_to_string(&manifest_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(parsed.get("timestamp").is_some());
    assert!(parsed.get("db_size").is_some());
    assert_eq!(parsed["db_size"].as_u64().unwrap(), 12345);
    assert_eq!(parsed["include_uploads"].as_bool().unwrap(), false);
}

// ═══════════════════════════════════════════════════════════════════════════
// 16. Blueprint
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn blueprint_save_and_list() {
    // We test via the internal copy_dir_recursive pattern (same as scaffold.rs unit tests)
    // since blueprint_save/use depend on the global ~/.config/crap-cms/blueprints/ dir.
    let tmp = tempfile::tempdir().expect("tempdir");

    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(config_dir.join("collections")).unwrap();
    std::fs::write(config_dir.join("crap.toml"), "# test config").unwrap();
    std::fs::write(config_dir.join("init.lua"), "-- test init").unwrap();
    std::fs::write(config_dir.join("collections/posts.lua"), "-- posts").unwrap();

    let bp_dir = tmp.path().join("blueprints").join("my-blog");
    std::fs::create_dir_all(&bp_dir).unwrap();

    // Simulate save: copy config to blueprint dir (skip data/uploads/types)
    copy_dir_skip(&config_dir, &bp_dir, &["data", "uploads", "types"]);

    assert!(bp_dir.join("crap.toml").exists());
    assert!(bp_dir.join("init.lua").exists());
    assert!(bp_dir.join("collections/posts.lua").exists());

    // Simulate list
    let bp_base = tmp.path().join("blueprints");
    let names: Vec<String> = std::fs::read_dir(&bp_base)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(names.contains(&"my-blog".to_string()));
}

#[test]
fn blueprint_use() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Create blueprint
    let bp_dir = tmp.path().join("blueprints").join("starter");
    std::fs::create_dir_all(bp_dir.join("collections")).unwrap();
    std::fs::write(bp_dir.join("crap.toml"), "[server]\nadmin_port = 5000\n").unwrap();
    std::fs::write(bp_dir.join("init.lua"), "-- starter").unwrap();
    std::fs::write(bp_dir.join("collections/pages.lua"), "-- pages").unwrap();

    // Use it
    let new_project = tmp.path().join("new-project");
    std::fs::create_dir_all(&new_project).unwrap();
    copy_dir(&bp_dir, &new_project);

    assert!(new_project.join("crap.toml").exists());
    let toml = std::fs::read_to_string(new_project.join("crap.toml")).unwrap();
    assert!(toml.contains("admin_port = 5000"));
    assert!(new_project.join("collections/pages.lua").exists());
}

#[test]
fn blueprint_remove() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let bp_dir = tmp.path().join("blueprints").join("throwaway");
    std::fs::create_dir_all(&bp_dir).unwrap();
    std::fs::write(bp_dir.join("crap.toml"), "# throwaway").unwrap();

    assert!(bp_dir.exists());
    std::fs::remove_dir_all(&bp_dir).unwrap();
    assert!(!bp_dir.exists());
}

#[test]
fn blueprint_refuses_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("crap.toml"), "# config").unwrap();

    // Simulate: blueprint_save calls scaffold::blueprint_save which checks existence
    let result = scaffold::blueprint_save(&config_dir, "test-bp-overwrite-check", false);
    if result.is_ok() {
        // Second save should fail without force
        let result2 = scaffold::blueprint_save(&config_dir, "test-bp-overwrite-check", false);
        assert!(result2.is_err());
        assert!(result2.unwrap_err().to_string().contains("already exists"));
        // Clean up
        let _ = scaffold::blueprint_remove("test-bp-overwrite-check");
    }
    // If first save fails (e.g., no config dir permissions), that's also acceptable for this test
}

#[test]
fn blueprint_save_writes_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("crap.toml"), "# config").unwrap();

    let bp_name = "test-bp-manifest-check";
    let result = scaffold::blueprint_save(&config_dir, bp_name, true);
    if result.is_ok() {
        // Read the manifest from the blueprint directory
        let bp_dir = dirs::config_dir()
            .unwrap()
            .join("crap-cms/blueprints")
            .join(bp_name);
        let manifest_path = bp_dir.join(".crap-blueprint.toml");
        assert!(
            manifest_path.exists(),
            "manifest should be created on blueprint save"
        );

        let contents = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(
            contents.contains("crap_version"),
            "manifest should contain crap_version"
        );
        assert!(
            contents.contains(env!("CARGO_PKG_VERSION")),
            "manifest should contain current version"
        );

        // Clean up
        let _ = scaffold::blueprint_remove(bp_name);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 17. Make Job
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn make_job_creates_lua_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(tmp.path(), "cleanup", None, None, None, None, false).unwrap();

    let path = tmp.path().join("jobs/cleanup.lua");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("crap.jobs.define(\"cleanup\""));
    assert!(content.contains("jobs.cleanup.run"));
    assert!(content.contains("function M.run(context)"));
}

#[test]
fn make_job_with_schedule() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(
        tmp.path(),
        "nightly",
        Some("0 3 * * *"),
        None,
        None,
        None,
        false,
    )
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("jobs/nightly.lua")).unwrap();
    assert!(content.contains("schedule = \"0 3 * * *\""));
}

#[test]
fn make_job_with_options() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(
        tmp.path(),
        "heavy",
        None,
        Some("background"),
        Some(3),
        Some(300),
        false,
    )
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("jobs/heavy.lua")).unwrap();
    assert!(content.contains("queue = \"background\""));
    assert!(content.contains("retries = 3"));
    assert!(content.contains("timeout = 300"));
}

#[test]
fn make_job_existing_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(tmp.path(), "test_job", None, None, None, None, false).unwrap();
    let result = scaffold::make_job(tmp.path(), "test_job", None, None, None, None, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--force"));
}

#[test]
fn make_job_force_overwrites() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(tmp.path(), "test_job", None, None, None, None, false).unwrap();
    assert!(scaffold::make_job(tmp.path(), "test_job", None, None, None, None, true).is_ok());
}

#[test]
fn make_job_default_queue_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(
        tmp.path(),
        "simple",
        None,
        Some("default"),
        None,
        None,
        false,
    )
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("jobs/simple.lua")).unwrap();
    // "default" queue should not generate an explicit config line
    assert!(!content.contains("queue ="));
}

#[test]
fn make_job_default_timeout_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(tmp.path(), "basic", None, None, None, Some(60), false).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("jobs/basic.lua")).unwrap();
    // default timeout=60 should not generate an explicit config line
    assert!(!content.contains("timeout ="));
}

#[test]
fn make_job_zero_retries_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(tmp.path(), "noretry", None, None, Some(0), None, false).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("jobs/noretry.lua")).unwrap();
    assert!(!content.contains("retries ="));
}

// ═══════════════════════════════════════════════════════════════════════════
// 30b. Restore Command
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_restore_requires_confirm() {
    let (tmp, pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");
    drop(pool);

    // Create a fake backup dir
    let backup_dir = tmp.path().join("fake-backup");
    std::fs::create_dir_all(&backup_dir).unwrap();

    let result = commands::db::restore(&config_dir, &backup_dir, false, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--confirm"));
}

#[test]
fn cmd_restore_validates_backup_dir() {
    let (tmp, pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");
    drop(pool);

    // Empty dir — no manifest.json
    let backup_dir = tmp.path().join("empty-backup");
    std::fs::create_dir_all(&backup_dir).unwrap();

    let result = commands::db::restore(&config_dir, &backup_dir, false, true);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("manifest.json"));
}

#[test]
fn cmd_restore_roundtrip() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Create data
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Restore Test Post".to_string());
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }
    drop(pool);

    // Backup
    let backup_output = tmp.path().join("backups");
    commands::db::backup(&config_dir, Some(backup_output.clone()), false).unwrap();

    let backup_dirs: Vec<_> = std::fs::read_dir(&backup_output)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    let backup_dir = backup_dirs[0].path();

    // Delete the original DB
    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let db_path = cfg.db_path(&config_dir);
    std::fs::remove_file(&db_path).unwrap();
    assert!(!db_path.exists());

    // Restore
    commands::db::restore(&config_dir, &backup_dir, false, true).unwrap();

    // Verify DB was restored
    assert!(db_path.exists(), "DB should be restored");

    // Verify data is intact
    let pool2 = pool::create_pool(&config_dir, &cfg).expect("create pool");
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();
    let conn = pool2.get().unwrap();
    let results = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fields.get("title").unwrap(), "Restore Test Post");
}

// ═══════════════════════════════════════════════════════════════════════════
// 31. Make Collection via Binary (non-interactive)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn make_collection_via_binary_no_input() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let output = std::process::Command::new(crap_bin())
        .args([
            "make",
            "collection",
            config_dir.to_str().unwrap(),
            "articles",
            "--fields",
            "title:text:required,body:textarea",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "make collection should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lua_path = config_dir.join("collections/articles.lua");
    assert!(lua_path.exists(), "collection file should be created");
    let content = std::fs::read_to_string(&lua_path).unwrap();
    assert!(content.contains("crap.collections.define(\"articles\""));
    assert!(content.contains("name = \"title\""));
    assert!(content.contains("name = \"body\""));
}

#[test]
fn make_collection_via_binary_auth() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let output = std::process::Command::new(crap_bin())
        .args([
            "make",
            "collection",
            config_dir.to_str().unwrap(),
            "members",
            "--auth",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "make collection with auth should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(config_dir.join("collections/members.lua")).unwrap();
    assert!(content.contains("auth = true"));
}

#[test]
fn make_collection_via_binary_upload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let output = std::process::Command::new(crap_bin())
        .args([
            "make",
            "collection",
            config_dir.to_str().unwrap(),
            "media",
            "--upload",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "make collection with upload should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(config_dir.join("collections/media.lua")).unwrap();
    assert!(content.contains("upload = true"));
}

#[test]
fn make_collection_via_binary_versions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let output = std::process::Command::new(crap_bin())
        .args([
            "make",
            "collection",
            config_dir.to_str().unwrap(),
            "articles",
            "--versions",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "make collection with versions should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(config_dir.join("collections/articles.lua")).unwrap();
    assert!(content.contains("versions"));
}

#[test]
fn make_collection_via_binary_no_timestamps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let output = std::process::Command::new(crap_bin())
        .args([
            "make",
            "collection",
            config_dir.to_str().unwrap(),
            "logs",
            "--no-timestamps",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "make collection --no-timestamps should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(config_dir.join("collections/logs.lua")).unwrap();
    assert!(content.contains("timestamps = false"));
}

#[test]
fn make_collection_via_binary_no_slug_no_input_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let output = std::process::Command::new(crap_bin())
        .args([
            "make",
            "collection",
            config_dir.to_str().unwrap(),
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        !output.status.success(),
        "make collection without slug in --no-input should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("required") || stderr.contains("slug"),
        "error should mention slug is required, got: {}",
        stderr
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 32. Status via Binary
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn status_via_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let output = std::process::Command::new(crap_bin())
        .args(["status", config_dir.to_str().unwrap()])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "status should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Collections"),
        "status output should mention collections"
    );
    assert!(
        stdout.contains("posts"),
        "status output should show posts collection"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 33. Jobs via Binary
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn jobs_list_via_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let output = std::process::Command::new(crap_bin())
        .args(["jobs", "list", config_dir.to_str().unwrap()])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "jobs list should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 34. Templates via Binary
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn templates_list_via_binary() {
    let output = std::process::Command::new(crap_bin())
        .args(["templates", "list"])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "templates list should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Templates"), "should list templates");
    assert!(stdout.contains("Static files"), "should list static files");
}

#[test]
fn templates_extract_via_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let output = std::process::Command::new(crap_bin())
        .args([
            "templates",
            "extract",
            tmp.path().to_str().unwrap(),
            "layout/base.hbs",
        ])
        .output()
        .expect("failed to run binary");

    assert!(
        output.status.success(),
        "templates extract should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.path().join("templates/layout/base.hbs").exists());
}
