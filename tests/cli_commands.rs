//! CLI command function tests for crap-cms.
//!
//! Tests for command library functions (sections 18-30):
//! direct Rust calls without invoking the binary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crap_cms::commands;
use crap_cms::config::CrapConfig;
use crap_cms::core::auth;
use crap_cms::db::{DbPool, migrate, pool, query};
use crap_cms::hooks;

// ── Helpers ──────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("cli_tests")
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

/// Create a user in an auth collection via query::create + update_password.
fn create_user(
    pool: &DbPool,
    def: &crap_cms::core::CollectionDefinition,
    email: &str,
    password: &str,
    extra_fields: &[(&str, &str)],
) -> crap_cms::core::Document {
    let mut data = HashMap::new();
    data.insert("email".to_string(), email.to_string());
    for (k, v) in extra_fields {
        data.insert(k.to_string(), v.to_string());
    }
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "users", def, &data, None).expect("create user");
    query::update_password(&tx, "users", &doc.id, password).expect("set password");
    tx.commit().expect("Commit");
    doc
}

// ── Blueprint helper ─────────────────────────────────────────────────────

#[allow(dead_code)]
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
// 18. Command Export/Import Functions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_export_all() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Seed some posts
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..3 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Export Post {}", i));
            query::create(&tx, "posts", def, &data, None).unwrap();
        }
        tx.commit().unwrap();
    }

    let output_path = tmp.path().join("export.json");
    commands::export::export(&config_dir, None, Some(output_path.clone())).unwrap();

    // Verify the JSON file structure
    let content = std::fs::read_to_string(&output_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let collections = parsed
        .get("collections")
        .expect("should have 'collections' key");
    let posts = collections
        .get("posts")
        .expect("should have 'posts' collection");
    let posts_arr = posts.as_array().unwrap();
    assert_eq!(posts_arr.len(), 3, "should have 3 posts");

    // Each post should have a title
    for post in posts_arr {
        assert!(post.get("title").is_some(), "each post should have a title");
        assert!(post.get("id").is_some(), "each post should have an id");
    }
}

#[test]
fn cmd_export_collection_filter() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Seed posts and a user
    {
        let reg = registry.read().unwrap();
        let posts_def = reg.get_collection("posts").unwrap();
        let users_def = reg.get_collection("users").unwrap();

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Filtered Post".to_string());
        query::create(&tx, "posts", posts_def, &data, None).unwrap();

        let mut udata = HashMap::new();
        udata.insert("email".to_string(), "filter@example.com".to_string());
        udata.insert("name".to_string(), "Filter User".to_string());
        query::create(&tx, "users", users_def, &udata, None).unwrap();

        tx.commit().unwrap();
    }

    let output_path = tmp.path().join("export_filtered.json");
    commands::export::export(
        &config_dir,
        Some("posts".to_string()),
        Some(output_path.clone()),
    )
    .unwrap();

    let content = std::fs::read_to_string(&output_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let collections = parsed.get("collections").unwrap().as_object().unwrap();
    assert!(collections.contains_key("posts"), "should contain posts");
    assert!(
        !collections.contains_key("users"),
        "should NOT contain users"
    );
    assert_eq!(collections["posts"].as_array().unwrap().len(), 1);
}

#[test]
fn cmd_export_nonexistent_errors() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    let output_path = tmp.path().join("export_bad.json");
    let result = commands::export::export(
        &config_dir,
        Some("nonexistent_collection".to_string()),
        Some(output_path),
    );
    assert!(
        result.is_err(),
        "exporting nonexistent collection should fail"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not found"),
        "error should mention 'not found', got: {}",
        err_msg
    );
}

#[test]
fn cmd_import_roundtrip() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Seed data
    let mut original_ids = Vec::new();
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..3 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Roundtrip {}", i));
            data.insert("status".to_string(), "published".to_string());
            let doc = query::create(&tx, "posts", def, &data, None).unwrap();
            original_ids.push(doc.id.clone());
        }
        tx.commit().unwrap();
    }

    // Export to file
    let export_path = tmp.path().join("roundtrip.json");
    commands::export::export(&config_dir, None, Some(export_path.clone())).unwrap();

    // Delete all posts
    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        for id in &original_ids {
            query::delete(&tx, "posts", id).unwrap();
        }
        tx.commit().unwrap();
    }

    // Verify empty
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let conn = pool.get().unwrap();
        let docs = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
        assert_eq!(docs.len(), 0, "posts should be empty after delete");
    }

    // Import from the exported file
    commands::export::import(&config_dir, &export_path, Some("posts".to_string())).unwrap();

    // Verify data restored
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let conn = pool.get().unwrap();
        let docs = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
        assert_eq!(docs.len(), 3, "should have 3 posts after import");
        for doc in &docs {
            assert!(
                original_ids.contains(&doc.id),
                "restored doc should have original ID"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 19. Command User Functions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_user_create_via_library() {
    let (_tmp, pool, registry) = full_setup();

    commands::user::user_create(
        &pool,
        &registry,
        "users",
        Some("lib_create@example.com".to_string()),
        Some("password123".to_string()),
        vec![("name".to_string(), "Lib User".to_string())],
        &crap_cms::config::PasswordPolicy::default(),
    )
    .unwrap();

    // Verify user was created in DB
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap();
    let conn = pool.get().unwrap();
    let found = query::find_by_email(&conn, "users", def, "lib_create@example.com")
        .unwrap()
        .expect("user should exist after create");
    assert_eq!(found.get_str("email"), Some("lib_create@example.com"));
    assert_eq!(found.get_str("name"), Some("Lib User"));

    // Verify password was set
    let hash = query::get_password_hash(&conn, "users", &found.id)
        .unwrap()
        .expect("password hash should exist");
    assert!(auth::verify_password("password123", hash.as_ref()).unwrap());
}

#[test]
fn cmd_user_create_extra_fields() {
    let (_tmp, pool, registry) = full_setup();

    commands::user::user_create(
        &pool,
        &registry,
        "users",
        Some("extra@example.com".to_string()),
        Some("secret456".to_string()),
        vec![
            ("name".to_string(), "Admin User".to_string()),
            ("role".to_string(), "admin".to_string()),
        ],
        &crap_cms::config::PasswordPolicy::default(),
    )
    .unwrap();

    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap();
    let conn = pool.get().unwrap();
    let found = query::find_by_email(&conn, "users", def, "extra@example.com")
        .unwrap()
        .expect("user should exist");
    assert_eq!(found.get_str("name"), Some("Admin User"));
    assert_eq!(found.get_str("role"), Some("admin"));
}

#[test]
fn cmd_user_create_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_create(
        &pool,
        &registry,
        "posts",
        Some("fail@example.com".to_string()),
        Some("password".to_string()),
        vec![],
        &crap_cms::config::PasswordPolicy::default(),
    );
    assert!(
        result.is_err(),
        "creating user in non-auth collection should fail"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not an auth collection"),
        "error should mention 'not an auth collection', got: {}",
        err_msg
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 20. Command Status/Typegen/Templates
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_typegen_via_library() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    let result = commands::typegen::run(&config_dir, "lua", None);
    assert!(
        result.is_ok(),
        "typegen lua should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_typegen_all_via_library() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    let result = commands::typegen::run(&config_dir, "all", None);
    assert!(
        result.is_ok(),
        "typegen all should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_typegen_invalid_lang_errors() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    let result = commands::typegen::run(&config_dir, "invalid_lang", None);
    assert!(result.is_err(), "typegen with invalid language should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Unknown language"),
        "error should mention 'Unknown language', got: {}",
        err_msg
    );
}

#[test]
fn cmd_typegen_custom_output_dir() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");
    let custom_output = tmp.path().join("custom_types");

    let result = commands::typegen::run(&config_dir, "lua", Some(&custom_output));
    assert!(
        result.is_ok(),
        "typegen with custom output should succeed: {:?}",
        result.err()
    );
    assert!(
        custom_output.join("generated.lua").exists(),
        "should write to custom output dir"
    );
    assert!(
        custom_output.join("crap.lua").exists(),
        "should write API types to custom output dir"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 21. Command DB Functions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_migrate_create() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    commands::db::migrate(
        &config_dir,
        commands::MigrateAction::Create {
            name: "test_migration".into(),
        },
    )
    .unwrap();

    let migrations_dir = config_dir.join("migrations");
    let files: Vec<_> = std::fs::read_dir(&migrations_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "should have created one migration file");
    let filename = files[0].file_name().to_string_lossy().to_string();
    assert!(
        filename.ends_with("_test_migration.lua"),
        "migration file should end with '_test_migration.lua', got: {}",
        filename
    );

    let content = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(content.contains("function M.up()"), "should have M.up()");
    assert!(
        content.contains("function M.down()"),
        "should have M.down()"
    );
}

#[test]
fn cmd_migrate_fresh_needs_confirm() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    let result = commands::db::migrate(
        &config_dir,
        commands::MigrateAction::Fresh { confirm: false },
    );
    assert!(result.is_err(), "migrate fresh without confirm should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("--confirm"),
        "error should mention '--confirm', got: {}",
        err_msg
    );
}

#[test]
fn cmd_backup_creates_snapshot() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Create some data so the DB has content
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Backup Test Post".to_string());
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Drop pool before backup to release DB connections
    drop(pool);

    let backup_output = tmp.path().join("backups");
    commands::db::backup(&config_dir, Some(backup_output.clone()), false).unwrap();

    // The backup command creates a timestamped subdirectory
    assert!(
        backup_output.exists(),
        "backup output directory should exist"
    );
    let backup_dirs: Vec<_> = std::fs::read_dir(&backup_output)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(backup_dirs.len(), 1, "should have one backup directory");

    let backup_dir = backup_dirs[0].path();
    assert!(
        backup_dir.join("crap.db").exists(),
        "backup should contain crap.db"
    );
    assert!(
        backup_dir.join("manifest.json").exists(),
        "backup should contain manifest.json"
    );

    // Verify manifest
    let manifest_content = std::fs::read_to_string(backup_dir.join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest_content).unwrap();
    assert!(
        manifest.get("timestamp").is_some(),
        "manifest should have timestamp"
    );
    assert!(
        manifest.get("db_size").is_some(),
        "manifest should have db_size"
    );
    assert!(
        manifest["db_size"].as_u64().unwrap() > 0,
        "db_size should be > 0"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 22. Command Jobs Functions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_jobs_trigger() {
    // Set up a project with a job definition
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a job Lua file
    let jobs_dir = config_dir.join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("cleanup.lua"),
        r#"
crap.jobs.define("cleanup", {
    handler = "jobs.cleanup.run",
    queue = "default",
})

local M = {}
function M.run(ctx)
    return { cleaned = true }
end
return M
"#,
    )
    .unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync schema");

    // Verify the job is registered
    {
        let reg = registry.read().unwrap();
        assert!(
            reg.get_job("cleanup").is_some(),
            "cleanup job should be registered"
        );
    }

    // Use the jobs command to trigger it
    commands::jobs::run(commands::JobsAction::Trigger {
        config: config_dir.clone(),
        slug: "cleanup".to_string(),
        data: None,
    })
    .unwrap();

    // Verify a job run was created in the DB
    let conn = db_pool.get().unwrap();
    let runs =
        crap_cms::db::query::jobs::list_job_runs(&conn, Some("cleanup"), None, 10, 0).unwrap();
    assert_eq!(runs.len(), 1, "should have one job run");
    assert_eq!(runs[0].slug, "cleanup");
    assert_eq!(runs[0].status, crap_cms::core::job::JobStatus::Pending);
}

// ═══════════════════════════════════════════════════════════════════════════
// 23. parse_key_val helper (commands/mod.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn parse_key_val_valid() {
    let (k, v) = commands::parse_key_val("name=Admin User").unwrap();
    assert_eq!(k, "name");
    assert_eq!(v, "Admin User");
}

#[test]
fn parse_key_val_empty_value() {
    let (k, v) = commands::parse_key_val("key=").unwrap();
    assert_eq!(k, "key");
    assert_eq!(v, "");
}

#[test]
fn parse_key_val_multiple_equals() {
    let (k, v) = commands::parse_key_val("expr=a=b").unwrap();
    assert_eq!(k, "expr");
    assert_eq!(v, "a=b");
}

#[test]
fn parse_key_val_no_equals() {
    let result = commands::parse_key_val("noequalssign");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("no `=` found"), "error: {}", err);
}

#[test]
fn parse_key_val_empty_key() {
    let (k, v) = commands::parse_key_val("=value").unwrap();
    assert_eq!(k, "");
    assert_eq!(v, "value");
}

// ═══════════════════════════════════════════════════════════════════════════
// 24. load_config_and_sync (commands/mod.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn load_config_and_sync_works() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let (pool, registry) = commands::load_config_and_sync(&config_dir).unwrap();

    // Verify pool works
    let conn = pool.get().expect("should get connection");
    drop(conn);

    // Verify registry has expected collections
    let reg = registry.read().unwrap();
    assert!(reg.get_collection("posts").is_some());
    assert!(reg.get_collection("users").is_some());
}

#[test]
fn load_config_and_sync_bad_dir() {
    let result = commands::load_config_and_sync(Path::new("/nonexistent/dir/config"));
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// 25. make.rs helper functions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn has_locales_enabled_false_by_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    assert!(!commands::make::has_locales_enabled(&config_dir));
}

#[test]
fn has_locales_enabled_true_with_locales() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Add locales to crap.toml
    let toml_path = config_dir.join("crap.toml");
    std::fs::write(
        &toml_path,
        r#"
[locale]
locales = ["en", "de"]
default_locale = "en"
"#,
    )
    .unwrap();

    assert!(commands::make::has_locales_enabled(&config_dir));
}

#[test]
fn has_locales_enabled_empty_array() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    std::fs::write(
        config_dir.join("crap.toml"),
        r#"
[locale]
locales = []
"#,
    )
    .unwrap();

    assert!(!commands::make::has_locales_enabled(&config_dir));
}

#[test]
fn has_locales_enabled_no_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // No crap.toml at all
    assert!(!commands::make::has_locales_enabled(tmp.path()));
}

#[test]
fn try_load_collection_slugs_returns_sorted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let slugs = commands::make::try_load_collection_slugs(&config_dir);
    assert!(slugs.is_some());
    let slugs = slugs.unwrap();
    assert!(slugs.contains(&"posts".to_string()));
    assert!(slugs.contains(&"users".to_string()));
    // Verify sorted
    let mut sorted = slugs.clone();
    sorted.sort();
    assert_eq!(slugs, sorted);
}

#[test]
fn try_load_collection_slugs_bad_dir() {
    let result = commands::make::try_load_collection_slugs(Path::new("/nonexistent/dir"));
    assert!(result.is_none());
}

#[test]
fn try_load_field_names_returns_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let names = commands::make::try_load_field_names(&config_dir, "posts");
    assert!(names.is_some());
    let names = names.unwrap();
    assert!(names.contains(&"title".to_string()));
    assert!(names.contains(&"status".to_string()));
    assert!(names.contains(&"content".to_string()));
}

#[test]
fn try_load_field_names_nonexistent_collection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let names = commands::make::try_load_field_names(&config_dir, "nonexistent");
    assert!(names.is_none());
}

#[test]
fn try_load_field_names_bad_dir() {
    let result = commands::make::try_load_field_names(Path::new("/nonexistent"), "posts");
    assert!(result.is_none());
}

#[test]
fn try_load_field_infos_returns_infos() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let infos = commands::make::try_load_field_infos(&config_dir, "posts");
    assert!(infos.is_some());
    let infos = infos.unwrap();
    // Should have title, status, content fields (but not array/blocks/group types which are filtered)
    let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"title"), "should have title field info");
    assert!(names.contains(&"status"), "should have status field info");

    // Check status has select_options
    let status_info = infos.iter().find(|i| i.name == "status").unwrap();
    assert_eq!(status_info.field_type, "select");
    assert!(status_info.select_options.contains(&"draft".to_string()));
    assert!(
        status_info
            .select_options
            .contains(&"published".to_string())
    );
}

#[test]
fn try_load_field_infos_nonexistent_collection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let infos = commands::make::try_load_field_infos(&config_dir, "nonexistent");
    assert!(infos.is_none());
}

#[test]
fn try_load_field_infos_bad_dir() {
    let result = commands::make::try_load_field_infos(Path::new("/nonexistent"), "posts");
    assert!(result.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// 26. User command functions (user.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_user_list() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    // Create some users
    create_user(
        &pool,
        &def,
        "alice@example.com",
        "pw123",
        &[("name", "Alice")],
    );
    create_user(&pool, &def, "bob@example.com", "pw456", &[("name", "Bob")]);

    // user_list should succeed
    let result = commands::user::user_list(&pool, &registry, "users");
    assert!(
        result.is_ok(),
        "user_list should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_user_list_empty() {
    let (_tmp, pool, registry) = full_setup();

    // No users yet — should succeed with "No users" message
    let result = commands::user::user_list(&pool, &registry, "users");
    assert!(
        result.is_ok(),
        "user_list on empty collection should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_user_list_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_list(&pool, &registry, "posts");
    assert!(
        result.is_err(),
        "user_list on non-auth collection should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {}", err);
}

#[test]
fn cmd_user_list_missing_collection_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_list(&pool, &registry, "nonexistent");
    assert!(
        result.is_err(),
        "user_list on missing collection should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"), "error: {}", err);
}
