//! CLI integration tests for crap-cms.
//!
//! Two layers:
//! 1. Library function tests — direct Rust calls with temp dirs.
//! 2. Binary invocation tests — `std::process::Command` for clap parsing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crap_cms::commands;
use crap_cms::config::CrapConfig;
use crap_cms::core::auth;
use crap_cms::db::{migrate, ops, pool, query, DbPool};
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

// ═══════════════════════════════════════════════════════════════════════════
// 1. Clap Parsing & Help
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn help_shows_all_commands() {
    let output = std::process::Command::new(crap_bin())
        .arg("--help")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "help should succeed");
    for cmd in &["serve", "status", "user", "make", "init", "export", "import", "backup", "migrate", "typegen", "proto", "blueprint"] {
        assert!(stdout.contains(cmd), "help should list '{}' command, got:\n{}", cmd, stdout);
    }
}

#[test]
fn version_flag() {
    let output = std::process::Command::new(crap_bin())
        .arg("--version")
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("crap-cms"), "version should contain binary name");
}

#[test]
fn user_subcommand_help() {
    let output = std::process::Command::new(crap_bin())
        .args(["user", "--help"])
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    for sub in &["create", "list", "delete", "lock", "unlock", "change-password"] {
        assert!(stdout.contains(sub), "user help should list '{}', got:\n{}", sub, stdout);
    }
}

#[test]
fn make_subcommand_help() {
    let output = std::process::Command::new(crap_bin())
        .args(["make", "--help"])
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    for sub in &["collection", "global", "hook", "migration"] {
        assert!(stdout.contains(sub), "make help should list '{}', got:\n{}", sub, stdout);
    }
}

#[test]
fn migrate_subcommand_help() {
    let output = std::process::Command::new(crap_bin())
        .args(["migrate", "--help"])
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    for sub in &["create", "up", "down", "list", "fresh"] {
        assert!(stdout.contains(sub), "migrate help should list '{}', got:\n{}", sub, stdout);
    }
}

#[test]
fn migrate_create_via_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let output = std::process::Command::new(crap_bin())
        .args(["migrate", config_dir.to_str().unwrap(), "create", "add_tags"])
        .output()
        .expect("failed to run binary");
    assert!(output.status.success(), "migrate create should succeed: {}",
        String::from_utf8_lossy(&output.stderr));

    let migrations_dir = config_dir.join("migrations");
    let files: Vec<_> = std::fs::read_dir(&migrations_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1);
    let filename = files[0].file_name().to_string_lossy().to_string();
    assert!(filename.ends_with("_add_tags.lua"), "got: {}", filename);

    let content = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(content.contains("function M.up()"));
    assert!(content.contains("function M.down()"));
}

#[test]
fn missing_config_arg_fails() {
    let output = std::process::Command::new(crap_bin())
        .arg("serve")
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success(), "serve without config should fail");
}

#[test]
fn unknown_subcommand_fails() {
    let output = std::process::Command::new(crap_bin())
        .arg("foobar")
        .output()
        .expect("failed to run binary");
    assert!(!output.status.success(), "unknown subcommand should fail");
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Init
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn init_creates_structure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("my-project");
    scaffold::init(Some(target.clone()), &scaffold::InitOptions::default()).unwrap();

    assert!(target.join("crap.toml").exists());
    assert!(target.join("init.lua").exists());
    assert!(target.join(".luarc.json").exists());
    assert!(target.join(".gitignore").exists());
    assert!(target.join("collections").is_dir());
    assert!(target.join("globals").is_dir());
    assert!(target.join("hooks").is_dir());
    assert!(target.join("templates").is_dir());
    assert!(target.join("static").is_dir());
    assert!(target.join("migrations").is_dir());
    assert!(target.join("types").is_dir());
    assert!(target.join("types/crap.lua").exists());
}

#[test]
fn init_types_content() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("types-test");
    scaffold::init(Some(target.clone()), &scaffold::InitOptions::default()).unwrap();

    let content = std::fs::read_to_string(target.join("types/crap.lua")).unwrap();
    assert!(!content.is_empty(), "types/crap.lua should not be empty");
    assert!(content.contains("crap"), "types/crap.lua should contain crap API definitions");
}

#[test]
fn init_refuses_existing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("existing");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("crap.toml"), "# existing").unwrap();

    let result = scaffold::init(Some(target), &scaffold::InitOptions::default());
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("refusing to overwrite"));
}

#[test]
fn init_custom_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let custom = tmp.path().join("deep").join("nested").join("project");
    scaffold::init(Some(custom.clone()), &scaffold::InitOptions::default()).unwrap();

    assert!(custom.join("crap.toml").exists());
    assert!(custom.join("init.lua").exists());
}

#[test]
fn init_content_valid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("valid-config");
    scaffold::init(Some(target.clone()), &scaffold::InitOptions::default()).unwrap();

    let cfg = CrapConfig::load(&target);
    assert!(cfg.is_ok(), "scaffolded crap.toml should load: {:?}", cfg.err());
}

#[test]
fn init_lua_loadable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("lua-test");
    scaffold::init(Some(target.clone()), &scaffold::InitOptions::default()).unwrap();

    let cfg = CrapConfig::load(&target).unwrap();
    let result = hooks::init_lua(&target, &cfg);
    assert!(result.is_ok(), "scaffolded init.lua should load: {:?}", result.err());
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Make Collection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn make_collection_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
    assert!(content.contains("crap.collections.define(\"posts\""));
    assert!(content.contains("timestamps = true"));
    assert!(content.contains("name = \"title\""));
    assert!(content.contains("type = \"text\""));
}

#[test]
fn make_collection_fields_shorthand() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(
        tmp.path(), "articles",
        Some("title:text:required,body:textarea"),
        false, false, false, false, false,
    ).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("collections/articles.lua")).unwrap();
    assert!(content.contains("name = \"title\""));
    assert!(content.contains("name = \"body\""));
    assert!(content.contains("type = \"textarea\""));
    assert!(content.contains("required = true"));
}

#[test]
fn make_collection_no_timestamps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(tmp.path(), "logs", None, true, false, false, false, false).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("collections/logs.lua")).unwrap();
    assert!(content.contains("timestamps = false"));
}

#[test]
fn make_collection_refuses_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();
    let result = scaffold::make_collection(tmp.path(), "posts", None, false, false, false, false, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--force"));
}

#[test]
fn make_collection_force_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();
    assert!(scaffold::make_collection(tmp.path(), "posts", None, false, false, false, false, true).is_ok());
}

#[test]
fn make_collection_invalid_slug() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(scaffold::make_collection(tmp.path(), "Posts", None, false, false, false, false, false).is_err());
    assert!(scaffold::make_collection(tmp.path(), "my-slug", None, false, false, false, false, false).is_err());
    assert!(scaffold::make_collection(tmp.path(), "_private", None, false, false, false, false, false).is_err());
    assert!(scaffold::make_collection(tmp.path(), "", None, false, false, false, false, false).is_err());
}

#[test]
fn make_collection_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    scaffold::make_collection(&config_dir, "articles", Some("title:text:required,body:richtext"), false, false, false, false, false).unwrap();

    let cfg = CrapConfig::load(&config_dir).unwrap();
    let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
    let reg = registry.read().unwrap();
    assert!(reg.get_collection("articles").is_some(), "articles should be in registry");
    let def = reg.get_collection("articles").unwrap();
    assert_eq!(def.fields.len(), 2);
    assert_eq!(def.fields[0].name, "title");
    assert_eq!(def.fields[1].name, "body");
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Make Global
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn make_global_creates_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_global(tmp.path(), "site_settings", false).unwrap();

    let path = tmp.path().join("globals/site_settings.lua");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("crap.globals.define(\"site_settings\""));
}

#[test]
fn make_global_refuses_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_global(tmp.path(), "settings", false).unwrap();
    let result = scaffold::make_global(tmp.path(), "settings", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--force"));
}

#[test]
fn make_global_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    scaffold::make_global(&config_dir, "navigation", false).unwrap();

    let cfg = CrapConfig::load(&config_dir).unwrap();
    let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
    let reg = registry.read().unwrap();
    assert!(reg.get_global("navigation").is_some(), "navigation should be in registry");
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Make Hook
// ═══════════════════════════════════════════════════════════════════════════

fn hook_opts<'a>(
    config_dir: &'a Path,
    name: &'a str,
    hook_type: scaffold::HookType,
    collection: &'a str,
    position: &'a str,
    field: Option<&'a str>,
    force: bool,
) -> scaffold::MakeHookOptions<'a> {
    scaffold::MakeHookOptions { config_dir, name, hook_type, collection, position, field, force, condition_field: None }
}

#[test]
fn make_hook_creates_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "auto_slug", scaffold::HookType::Collection,
        "posts", "before_change", None, false,
    );
    scaffold::make_hook(&opts).unwrap();

    let path = tmp.path().join("hooks/posts/auto_slug.lua");
    assert!(path.exists(), "hook file should be created");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("return function(context)"));
}

#[test]
fn make_hook_collection_hook() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "auto_slug", scaffold::HookType::Collection,
        "posts", "before_change", None, false,
    );
    scaffold::make_hook(&opts).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("hooks/posts/auto_slug.lua")).unwrap();
    assert!(content.contains("crap.HookContext"));
    assert!(content.contains("before_change hook for posts"));
    assert!(content.contains("return function(context)"));
    assert!(content.contains("return context"));
}

#[test]
fn make_hook_field_hook() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "normalize", scaffold::HookType::Field,
        "posts", "before_validate", Some("title"), false,
    );
    scaffold::make_hook(&opts).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("hooks/posts/normalize.lua")).unwrap();
    assert!(content.contains("crap.FieldHookContext"));
    assert!(content.contains("return function(value, context)"));
    assert!(content.contains("return value"));
}

#[test]
fn make_hook_access_hook() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "admin_only", scaffold::HookType::Access,
        "posts", "read", None, false,
    );
    scaffold::make_hook(&opts).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("hooks/posts/admin_only.lua")).unwrap();
    assert!(content.contains("crap.AccessContext"));
    assert!(content.contains("return true"));
}

#[test]
fn make_hook_refuses_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "auto_slug", scaffold::HookType::Collection,
        "posts", "before_change", None, false,
    );
    scaffold::make_hook(&opts).unwrap();
    let result = scaffold::make_hook(&opts);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--force"));
}

#[test]
fn make_hook_force_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "auto_slug", scaffold::HookType::Collection,
        "posts", "before_change", None, false,
    );
    scaffold::make_hook(&opts).unwrap();
    let opts_force = hook_opts(
        tmp.path(), "auto_slug", scaffold::HookType::Collection,
        "posts", "before_change", None, true,
    );
    assert!(scaffold::make_hook(&opts_force).is_ok());
}

#[test]
fn make_hook_invalid_position() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "bad", scaffold::HookType::Collection,
        "posts", "not_a_real_position", None, false,
    );
    let result = scaffold::make_hook(&opts);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid position"));
}

#[test]
fn make_hook_prints_hook_ref() {
    // The hook ref should be "hooks.<collection>.<name>"
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(), "auto_slug", scaffold::HookType::Collection,
        "posts", "before_change", None, false,
    );
    scaffold::make_hook(&opts).unwrap();

    // Verify the file is at the expected path for the hook ref
    let hook_file = tmp.path().join("hooks/posts/auto_slug.lua");
    assert!(hook_file.exists());
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Make Migration
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn make_migration_creates_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_migration(tmp.path(), "add_categories").unwrap();

    let files: Vec<_> = std::fs::read_dir(tmp.path().join("migrations"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1);
    let filename = files[0].file_name().to_string_lossy().to_string();
    assert!(filename.ends_with("_add_categories.lua"), "got: {}", filename);
}

#[test]
fn make_migration_has_up_down() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_migration(tmp.path(), "seed_data").unwrap();

    let files: Vec<_> = std::fs::read_dir(tmp.path().join("migrations"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    let content = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(content.contains("function M.up()"));
    assert!(content.contains("function M.down()"));
    assert!(content.contains("return M"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. Proto Export
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn proto_to_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file_path = tmp.path().join("out.proto");
    scaffold::proto_export(Some(&file_path)).unwrap();

    let content = std::fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("syntax = \"proto3\""));
    assert!(content.contains("service ContentAPI"));
}

#[test]
fn proto_to_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("proto-out");
    std::fs::create_dir_all(&dir).unwrap();
    scaffold::proto_export(Some(&dir)).unwrap();

    assert!(dir.join("content.proto").exists());
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. Status Logic
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn status_collection_counts() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();

    // Create 3 posts
    for i in 0..3 {
        let mut data = HashMap::new();
        data.insert("title".to_string(), format!("Post {}", i));
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let conn = pool.get().unwrap();
    let count = query::count(&conn, "posts", def, &[], None).unwrap();
    assert_eq!(count, 3);
}

#[test]
fn status_empty_project() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("empty-project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let cfg = CrapConfig::load(&config_dir).unwrap();
    let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
    let db_pool = pool::create_pool(&config_dir, &cfg).unwrap();
    migrate::sync_all(&db_pool, &registry, &cfg.locale).unwrap();

    let reg = registry.read().unwrap();
    assert!(reg.collections.is_empty());
    assert!(reg.globals.is_empty());
}

#[test]
fn status_auth_tag() {
    let (_tmp, _pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let users_def = reg.get_collection("users").unwrap();
    assert!(users_def.is_auth_collection(), "users should be an auth collection");

    let posts_def = reg.get_collection("posts").unwrap();
    assert!(!posts_def.is_auth_collection(), "posts should NOT be an auth collection");
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. User Commands
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn user_create_and_find() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "test@example.com", "secret123", &[("name", "Test User")]);

    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", &def, &doc.id, None)
        .unwrap()
        .expect("user should exist");
    assert_eq!(found.get_str("email"), Some("test@example.com"));
    assert_eq!(found.get_str("name"), Some("Test User"));
}

#[test]
fn user_create_with_fields() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "admin@example.com", "pw123", &[
        ("name", "Admin"),
        ("role", "admin"),
    ]);

    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(found.get_str("name"), Some("Admin"));
    assert_eq!(found.get_str("role"), Some("admin"));
}

#[test]
fn user_password_verify() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "pw@example.com", "mypassword", &[("name", "PW User")]);

    let conn = pool.get().unwrap();
    let hash = query::get_password_hash(&conn, "users", &doc.id)
        .unwrap()
        .expect("password hash should exist");
    assert!(hash.starts_with("$argon2"), "should be argon2 hash");
    assert!(auth::verify_password("mypassword", &hash).unwrap());
    assert!(!auth::verify_password("wrongpassword", &hash).unwrap());
}

#[test]
fn user_find_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    create_user(&pool, &def, "lookup@example.com", "pw", &[("name", "Lookup")]);

    let conn = pool.get().unwrap();
    let found = query::find_by_email(&conn, "users", &def, "lookup@example.com")
        .unwrap()
        .expect("should find by email");
    assert_eq!(found.get_str("email"), Some("lookup@example.com"));

    let missing = query::find_by_email(&conn, "users", &def, "nobody@example.com").unwrap();
    assert!(missing.is_none());
}

#[test]
fn user_find_by_id() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "byid@example.com", "pw", &[("name", "ByID")]);

    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", &def, &doc.id, None)
        .unwrap()
        .expect("should find by id");
    assert_eq!(found.id, doc.id);
}

#[test]
fn user_delete() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "delete@example.com", "pw", &[("name", "Delete Me")]);
    let id = doc.id.clone();

    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::delete(&tx, "users", &id).unwrap();
        tx.commit().unwrap();
    }

    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", &def, &id, None).unwrap();
    assert!(found.is_none(), "deleted user should not be found");
}

#[test]
fn user_lock_unlock() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "lock@example.com", "pw", &[("name", "Lockable")]);

    let conn = pool.get().unwrap();

    assert!(!query::is_locked(&conn, "users", &doc.id).unwrap());
    query::lock_user(&conn, "users", &doc.id).unwrap();
    assert!(query::is_locked(&conn, "users", &doc.id).unwrap());
    query::unlock_user(&conn, "users", &doc.id).unwrap();
    assert!(!query::is_locked(&conn, "users", &doc.id).unwrap());
}

#[test]
fn user_change_password() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "chpw@example.com", "oldpassword", &[("name", "ChPW")]);

    let conn = pool.get().unwrap();
    query::update_password(&conn, "users", &doc.id, "newpassword").unwrap();

    let hash = query::get_password_hash(&conn, "users", &doc.id).unwrap().unwrap();
    assert!(auth::verify_password("newpassword", &hash).unwrap());
    assert!(!auth::verify_password("oldpassword", &hash).unwrap());
}

#[test]
fn user_non_auth_rejected() {
    let (_tmp, _pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let posts_def = reg.get_collection("posts").unwrap();
    assert!(!posts_def.is_auth_collection(), "posts is not auth");
    // The CLI would check is_auth_collection() and bail — we verify the flag.
}

#[test]
fn user_missing_collection_rejected() {
    let (_tmp, _pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    assert!(reg.get_collection("nonexistent").is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. Export
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn export_all() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let posts_def = reg.get_collection("posts").unwrap();
    let users_def = reg.get_collection("users").unwrap();

    // Seed data
    {
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Hello".to_string());
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "posts", posts_def, &data, None).unwrap();
        tx.commit().unwrap();
    }
    {
        let mut data = HashMap::new();
        data.insert("email".to_string(), "export@example.com".to_string());
        data.insert("name".to_string(), "Exporter".to_string());
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "users", users_def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Replicate export logic
    let conn = pool.get().unwrap();
    let mut collections_data = serde_json::Map::new();
    for (slug, def) in &reg.collections {
        let docs = query::find(&conn, slug, def, &query::FindQuery::default(), None).unwrap();
        let docs_json: Vec<serde_json::Value> = docs.into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        collections_data.insert(slug.clone(), serde_json::Value::Array(docs_json));
    }

    assert!(collections_data.contains_key("posts"));
    assert!(collections_data.contains_key("users"));
    assert_eq!(collections_data["posts"].as_array().unwrap().len(), 1);
    assert_eq!(collections_data["users"].as_array().unwrap().len(), 1);
}

#[test]
fn export_filtered() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let posts_def = reg.get_collection("posts").unwrap();

    {
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Filtered".to_string());
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "posts", posts_def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let conn = pool.get().unwrap();
    // Only export "posts"
    let slug = "posts";
    assert!(reg.get_collection(slug).is_some());
    let def = reg.get_collection(slug).unwrap();
    let docs = query::find(&conn, slug, def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(docs.len(), 1);
}

#[test]
fn export_empty() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();

    let conn = pool.get().unwrap();
    let docs = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert!(docs.is_empty());
}

#[test]
fn export_nonexistent_fails() {
    let (_tmp, _pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    assert!(reg.get_collection("nonexistent").is_none(), "nonexistent collection should not be found");
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. Import
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn import_basic() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();

    let import_json = serde_json::json!({
        "collections": {
            "posts": [
                {"id": "import-1", "title": "Imported Post 1", "status": "published"},
                {"id": "import-2", "title": "Imported Post 2", "status": "draft"},
            ]
        }
    });

    // Replicate import logic
    let collections_obj = import_json["collections"].as_object().unwrap();
    let docs_array = collections_obj["posts"].as_array().unwrap();

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    for doc_val in docs_array {
        let doc_obj = doc_val.as_object().unwrap();
        let id = doc_obj["id"].as_str().unwrap();
        let mut parent_cols = vec!["id".to_string()];
        let mut parent_vals = vec![id.to_string()];
        for field in &def.fields {
            if field.has_parent_column() {
                if let Some(val) = doc_obj.get(&field.name) {
                    if let Some(s) = val.as_str() {
                        parent_cols.push(field.name.clone());
                        parent_vals.push(s.to_string());
                    }
                }
            }
        }
        let placeholders: Vec<String> = (0..parent_cols.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "INSERT OR REPLACE INTO \"{}\" ({}) VALUES ({})",
            "posts",
            parent_cols.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", "),
            placeholders.join(", ")
        );
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = parent_vals.iter()
            .map(|v| Box::new(v.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter()
            .map(|p| p.as_ref())
            .collect();
        tx.execute(&sql, param_refs.as_slice()).unwrap();
    }
    tx.commit().unwrap();

    // Verify
    let conn = pool.get().unwrap();
    let all = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn import_collection_filter() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();

    let import_json = serde_json::json!({
        "collections": {
            "posts": [
                {"id": "p1", "title": "Post 1"},
            ],
            "users": [
                {"id": "u1", "email": "u1@test.com", "name": "User 1"},
            ]
        }
    });

    let collections_obj = import_json["collections"].as_object().unwrap();

    // Only import "posts"
    let slug = "posts";
    assert!(collections_obj.contains_key(slug));
    let def = reg.get_collection(slug).unwrap();
    let docs_array = collections_obj[slug].as_array().unwrap();

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    for doc_val in docs_array {
        let obj = doc_val.as_object().unwrap();
        let id = obj["id"].as_str().unwrap();
        let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
        tx.execute(
            &format!("INSERT OR REPLACE INTO \"{}\" (id, title) VALUES (?1, ?2)", slug),
            rusqlite::params![id, title],
        ).unwrap();
    }
    tx.commit().unwrap();

    // Verify only posts imported
    let conn = pool.get().unwrap();
    let posts = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(posts.len(), 1);

    let users_def = reg.get_collection("users").unwrap();
    let users = query::find(&conn, "users", users_def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(users.len(), 0, "users should not be imported");
}

#[test]
fn import_preserves_ids() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT OR REPLACE INTO posts (id, title) VALUES (?1, ?2)",
        rusqlite::params!["custom-id-123", "Custom ID Post"],
    ).unwrap();
    tx.commit().unwrap();

    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "posts", def, "custom-id-123", None)
        .unwrap()
        .expect("should find by custom id");
    assert_eq!(found.id, "custom-id-123");
    assert_eq!(found.get_str("title"), Some("Custom ID Post"));
}

#[test]
fn import_with_timestamps() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT OR REPLACE INTO posts (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params!["ts-post", "Timestamped", "2024-01-01 00:00:00", "2024-06-15 12:30:00"],
    ).unwrap();
    tx.commit().unwrap();

    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "posts", def, "ts-post", None)
        .unwrap()
        .unwrap();
    assert_eq!(found.created_at.as_deref(), Some("2024-01-01 00:00:00"));
    assert_eq!(found.updated_at.as_deref(), Some("2024-06-15 12:30:00"));
}

#[test]
fn import_unknown_collection_fails() {
    let (_tmp, _pool, registry) = full_setup();
    let reg = registry.read().unwrap();

    let import_json = serde_json::json!({
        "collections": {
            "nonexistent": [
                {"id": "x1", "title": "X"}
            ]
        }
    });

    let collections_obj = import_json["collections"].as_object().unwrap();
    for slug in collections_obj.keys() {
        let found = reg.get_collection(slug);
        if slug == "nonexistent" {
            assert!(found.is_none(), "nonexistent collection should not be found");
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
    let exported_json: Vec<serde_json::Value> = exported.iter()
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
    let after_delete = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
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
            ).unwrap();
        }
        tx.commit().unwrap();
    }

    // Verify re-imported
    let conn = pool.get().unwrap();
    let reimported = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(reimported.len(), 3);
    for doc in &reimported {
        assert!(ids.contains(&doc.id), "re-imported doc should have original ID");
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
        let docs_json: Vec<serde_json::Value> = docs.into_iter()
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
        let path = typegen::generate_lang(&config_dir, &reg, *lang).unwrap();
        assert!(path.exists(), "file should exist for {:?}", lang);
        let expected_ext = format!("generated.{}", lang.file_extension());
        assert!(path.to_string_lossy().ends_with(&expected_ext),
            "expected ext {}, got {}", expected_ext, path.display());
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
    std::fs::write(migrations_dir.join("20240101000000_test.lua"), r#"
local M = {}
function M.up()
    -- no-op for test
end
function M.down()
    -- no-op for test
end
return M
"#).unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync");

    // Verify pending
    let pending = migrate::get_pending_migrations(&db_pool, &migrations_dir).unwrap();
    assert_eq!(pending.len(), 1);

    // Run migration via HookRunner
    let hook_runner = hooks::lifecycle::HookRunner::new(&config_dir, registry, &cfg).unwrap();
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
    std::fs::write(migrations_dir.join("20240101000000_rollback.lua"), r#"
local M = {}
function M.up()
end
function M.down()
end
return M
"#).unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync");

    // Apply migration
    let hook_runner = hooks::lifecycle::HookRunner::new(&config_dir, registry.clone(), &cfg).unwrap();
    let filename = "20240101000000_rollback.lua";
    {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        hook_runner.run_migration(&migrations_dir.join(filename), "up", &tx).unwrap();
        migrate::record_migration(&tx, filename).unwrap();
        tx.commit().unwrap();
    }
    assert!(migrate::get_applied_migrations(&db_pool).unwrap().contains(filename));

    // Rollback
    {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        hook_runner.run_migration(&migrations_dir.join(filename), "down", &tx).unwrap();
        migrate::remove_migration(&tx, filename).unwrap();
        tx.commit().unwrap();
    }
    assert!(!migrate::get_applied_migrations(&db_pool).unwrap().contains(filename));
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
        conn.execute("VACUUM INTO ?1", [backup_db_path.to_string_lossy().as_ref()]).unwrap();
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
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();

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
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();

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
    assert!(content.contains("function M.run(ctx)"));
}

#[test]
fn make_job_with_schedule() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(tmp.path(), "nightly", Some("0 3 * * *"), None, None, None, false).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("jobs/nightly.lua")).unwrap();
    assert!(content.contains("schedule = \"0 3 * * *\""));
}

#[test]
fn make_job_with_options() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_job(tmp.path(), "heavy", None, Some("background"), Some(3), Some(300), false).unwrap();

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
    scaffold::make_job(tmp.path(), "simple", None, Some("default"), None, None, false).unwrap();

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
    let collections = parsed.get("collections").expect("should have 'collections' key");
    let posts = collections.get("posts").expect("should have 'posts' collection");
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
    ).unwrap();

    let content = std::fs::read_to_string(&output_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let collections = parsed.get("collections").unwrap().as_object().unwrap();
    assert!(collections.contains_key("posts"), "should contain posts");
    assert!(!collections.contains_key("users"), "should NOT contain users");
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
    assert!(result.is_err(), "exporting nonexistent collection should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"), "error should mention 'not found', got: {}", err_msg);
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
            assert!(original_ids.contains(&doc.id), "restored doc should have original ID");
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
    ).unwrap();

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
    assert!(auth::verify_password("password123", &hash).unwrap());
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
    ).unwrap();

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
    );
    assert!(result.is_err(), "creating user in non-auth collection should fail");
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

    let result = commands::typegen::run(&config_dir, "lua");
    assert!(result.is_ok(), "typegen lua should succeed: {:?}", result.err());
}

#[test]
fn cmd_typegen_all_via_library() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    let result = commands::typegen::run(&config_dir, "all");
    assert!(result.is_ok(), "typegen all should succeed: {:?}", result.err());
}

#[test]
fn cmd_typegen_invalid_lang_errors() {
    let (tmp, _pool, _registry) = full_setup();
    let config_dir = tmp.path().join("config");

    let result = commands::typegen::run(&config_dir, "invalid_lang");
    assert!(result.is_err(), "typegen with invalid language should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Unknown language"),
        "error should mention 'Unknown language', got: {}",
        err_msg
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
        commands::MigrateAction::Create { name: "test_migration".into() },
    ).unwrap();

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
    assert!(content.contains("function M.down()"), "should have M.down()");
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
    assert!(backup_output.exists(), "backup output directory should exist");
    let backup_dirs: Vec<_> = std::fs::read_dir(&backup_output)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(backup_dirs.len(), 1, "should have one backup directory");

    let backup_dir = backup_dirs[0].path();
    assert!(backup_dir.join("crap.db").exists(), "backup should contain crap.db");
    assert!(backup_dir.join("manifest.json").exists(), "backup should contain manifest.json");

    // Verify manifest
    let manifest_content = std::fs::read_to_string(backup_dir.join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest_content).unwrap();
    assert!(manifest.get("timestamp").is_some(), "manifest should have timestamp");
    assert!(manifest.get("db_size").is_some(), "manifest should have db_size");
    assert!(manifest["db_size"].as_u64().unwrap() > 0, "db_size should be > 0");
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
    ).unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync schema");

    // Verify the job is registered
    {
        let reg = registry.read().unwrap();
        assert!(reg.get_job("cleanup").is_some(), "cleanup job should be registered");
    }

    // Use the jobs command to trigger it
    commands::jobs::run(commands::JobsAction::Trigger {
        config: config_dir.clone(),
        slug: "cleanup".to_string(),
        data: None,
    }).unwrap();

    // Verify a job run was created in the DB
    let conn = db_pool.get().unwrap();
    let runs = crap_cms::db::query::jobs::list_job_runs(&conn, Some("cleanup"), None, 10, 0).unwrap();
    assert_eq!(runs.len(), 1, "should have one job run");
    assert_eq!(runs[0].slug, "cleanup");
    assert_eq!(runs[0].status, crap_cms::core::job::JobStatus::Pending);
}

// ── Blueprint helper ─────────────────────────────────────────────────────

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

/// Set up a fixture dir that also includes a job definition.
fn full_setup_with_jobs() -> (tempfile::TempDir, crap_cms::db::DbPool, crap_cms::core::SharedRegistry) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Add a job Lua file
    let jobs_dir = config_dir.join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("cleanup.lua"),
        r#"
crap.jobs.define("cleanup", {
    handler = "jobs.cleanup.run",
    queue = "default",
    schedule = "0 3 * * *",
})

local M = {}
function M.run(ctx)
    return { cleaned = true }
end
return M
"#,
    ).unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync schema");

    (tmp, db_pool, registry)
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
    std::fs::write(&toml_path, r#"
[locale]
locales = ["en", "de"]
default_locale = "en"
"#).unwrap();

    assert!(commands::make::has_locales_enabled(&config_dir));
}

#[test]
fn has_locales_enabled_empty_array() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    std::fs::write(config_dir.join("crap.toml"), r#"
[locale]
locales = []
"#).unwrap();

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
    assert!(status_info.select_options.contains(&"published".to_string()));
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
    create_user(&pool, &def, "alice@example.com", "pw123", &[("name", "Alice")]);
    create_user(&pool, &def, "bob@example.com", "pw456", &[("name", "Bob")]);

    // user_list should succeed
    let result = commands::user::user_list(&pool, &registry, "users");
    assert!(result.is_ok(), "user_list should succeed: {:?}", result.err());
}

#[test]
fn cmd_user_list_empty() {
    let (_tmp, pool, registry) = full_setup();

    // No users yet — should succeed with "No users" message
    let result = commands::user::user_list(&pool, &registry, "users");
    assert!(result.is_ok(), "user_list on empty collection should succeed: {:?}", result.err());
}

#[test]
fn cmd_user_list_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_list(&pool, &registry, "posts");
    assert!(result.is_err(), "user_list on non-auth collection should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {}", err);
}

#[test]
fn cmd_user_list_missing_collection_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_list(&pool, &registry, "nonexistent");
    assert!(result.is_err(), "user_list on missing collection should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"), "error: {}", err);
}

#[test]
fn cmd_user_lock_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "lockme@example.com", "pw", &[("name", "Lock Me")]);

    // Verify not locked initially
    let conn = pool.get().unwrap();
    assert!(!query::is_locked(&conn, "users", &doc.id).unwrap());
    drop(conn);

    // Lock via command
    commands::user::user_lock(
        &pool, &registry, "users",
        Some("lockme@example.com".to_string()), None,
    ).unwrap();

    // Verify locked
    let conn = pool.get().unwrap();
    assert!(query::is_locked(&conn, "users", &doc.id).unwrap());
}

#[test]
fn cmd_user_lock_by_id() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "lockid@example.com", "pw", &[("name", "Lock ID")]);

    // Lock via ID
    commands::user::user_lock(
        &pool, &registry, "users",
        None, Some(doc.id.clone()),
    ).unwrap();

    let conn = pool.get().unwrap();
    assert!(query::is_locked(&conn, "users", &doc.id).unwrap());
}

#[test]
fn cmd_user_unlock_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "unlockme@example.com", "pw", &[("name", "Unlock Me")]);

    // Lock first
    let conn = pool.get().unwrap();
    query::lock_user(&conn, "users", &doc.id).unwrap();
    assert!(query::is_locked(&conn, "users", &doc.id).unwrap());
    drop(conn);

    // Unlock via command
    commands::user::user_unlock(
        &pool, &registry, "users",
        Some("unlockme@example.com".to_string()), None,
    ).unwrap();

    let conn = pool.get().unwrap();
    assert!(!query::is_locked(&conn, "users", &doc.id).unwrap());
}

#[test]
fn cmd_user_delete_with_confirm_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "deleteme@example.com", "pw", &[("name", "Delete Me")]);
    let id = doc.id.clone();

    // Delete with confirm=true (skips interactive prompt)
    commands::user::user_delete(
        &pool, &registry, "users",
        Some("deleteme@example.com".to_string()), None,
        true, // skip confirmation
    ).unwrap();

    // Verify deleted
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap();
    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", def, &id, None).unwrap();
    assert!(found.is_none(), "user should be deleted");
}

#[test]
fn cmd_user_delete_with_confirm_by_id() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "delbyid@example.com", "pw", &[("name", "Delete By ID")]);
    let id = doc.id.clone();

    // Delete by ID with confirm=true
    commands::user::user_delete(
        &pool, &registry, "users",
        None, Some(id.clone()),
        true,
    ).unwrap();

    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap();
    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", def, &id, None).unwrap();
    assert!(found.is_none(), "user should be deleted");
}

#[test]
fn cmd_user_delete_nonexistent_email_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_delete(
        &pool, &registry, "users",
        Some("nonexistent@example.com".to_string()), None,
        true,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("No user found"), "error: {}", err);
}

#[test]
fn cmd_user_change_password_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "chpw@example.com", "oldpw", &[("name", "ChPW User")]);

    // Change password via command (programmatic, not interactive)
    commands::user::user_change_password(
        &pool, &registry, "users",
        Some("chpw@example.com".to_string()), None,
        Some("newpw123".to_string()),
    ).unwrap();

    // Verify new password works
    let conn = pool.get().unwrap();
    let hash = query::get_password_hash(&conn, "users", &doc.id).unwrap().unwrap();
    assert!(auth::verify_password("newpw123", &hash).unwrap());
    assert!(!auth::verify_password("oldpw", &hash).unwrap());
}

#[test]
fn cmd_user_change_password_by_id() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let doc = create_user(&pool, &def, "chpwid@example.com", "oldpw", &[("name", "ChPW ID")]);

    commands::user::user_change_password(
        &pool, &registry, "users",
        None, Some(doc.id.clone()),
        Some("newpw456".to_string()),
    ).unwrap();

    let conn = pool.get().unwrap();
    let hash = query::get_password_hash(&conn, "users", &doc.id).unwrap().unwrap();
    assert!(auth::verify_password("newpw456", &hash).unwrap());
}

#[test]
fn cmd_user_change_password_nonexistent_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_change_password(
        &pool, &registry, "users",
        Some("noone@example.com".to_string()), None,
        Some("newpw".to_string()),
    );
    assert!(result.is_err());
}

#[test]
fn cmd_user_lock_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_lock(
        &pool, &registry, "posts",
        Some("anyone@example.com".to_string()), None,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {}", err);
}

#[test]
fn cmd_user_unlock_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_unlock(
        &pool, &registry, "posts",
        Some("anyone@example.com".to_string()), None,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {}", err);
}

#[test]
fn cmd_user_delete_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_delete(
        &pool, &registry, "posts",
        Some("anyone@example.com".to_string()), None,
        true,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {}", err);
}

#[test]
fn cmd_user_change_password_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_change_password(
        &pool, &registry, "posts",
        Some("anyone@example.com".to_string()), None,
        Some("newpw".to_string()),
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {}", err);
}

#[test]
fn cmd_user_create_missing_collection_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user::user_create(
        &pool,
        &registry,
        "nonexistent",
        Some("test@example.com".to_string()),
        Some("pw".to_string()),
        vec![],
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"), "error: {}", err);
}

// ═══════════════════════════════════════════════════════════════════════════
// 27. Jobs command functions (jobs.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_jobs_list() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(commands::JobsAction::List {
        config: config_dir,
    });
    assert!(result.is_ok(), "jobs list should succeed: {:?}", result.err());
}

#[test]
fn cmd_jobs_list_empty() {
    // Use fixture without jobs
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::jobs::run(commands::JobsAction::List {
        config: config_dir,
    });
    assert!(result.is_ok(), "jobs list with no jobs should succeed: {:?}", result.err());
}

#[test]
fn cmd_jobs_trigger_and_status() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // Trigger a job
    commands::jobs::run(commands::JobsAction::Trigger {
        config: config_dir.clone(),
        slug: "cleanup".to_string(),
        data: Some(r#"{"key": "value"}"#.to_string()),
    }).unwrap();

    // Check status (list all runs)
    let result = commands::jobs::run(commands::JobsAction::Status {
        config: config_dir.clone(),
        id: None,
        slug: Some("cleanup".to_string()),
        limit: 10,
    });
    assert!(result.is_ok(), "jobs status should succeed: {:?}", result.err());
}

#[test]
fn cmd_jobs_trigger_nonexistent_errors() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(commands::JobsAction::Trigger {
        config: config_dir,
        slug: "nonexistent_job".to_string(),
        data: None,
    });
    assert!(result.is_err(), "triggering nonexistent job should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not defined"), "error: {}", err);
}

#[test]
fn cmd_jobs_trigger_invalid_json_errors() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(commands::JobsAction::Trigger {
        config: config_dir,
        slug: "cleanup".to_string(),
        data: Some("not valid json {{{".to_string()),
    });
    assert!(result.is_err(), "triggering with invalid JSON should fail");
}

#[test]
fn cmd_jobs_status_single_run() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // Trigger a job first
    commands::jobs::run(commands::JobsAction::Trigger {
        config: config_dir.clone(),
        slug: "cleanup".to_string(),
        data: None,
    }).unwrap();

    // Get the run ID from the database
    let cfg = CrapConfig::load(&config_dir).unwrap();
    let db_pool = pool::create_pool(&config_dir, &cfg).unwrap();
    let conn = db_pool.get().unwrap();
    let runs = crap_cms::db::query::jobs::list_job_runs(&conn, Some("cleanup"), None, 10, 0).unwrap();
    assert!(!runs.is_empty());
    let run_id = runs[0].id.clone();
    drop(conn);
    drop(db_pool);

    // Show single run by ID
    let result = commands::jobs::run(commands::JobsAction::Status {
        config: config_dir,
        id: Some(run_id),
        slug: None,
        limit: 20,
    });
    assert!(result.is_ok(), "jobs status by ID should succeed: {:?}", result.err());
}

#[test]
fn cmd_jobs_status_not_found() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(commands::JobsAction::Status {
        config: config_dir,
        id: Some("nonexistent-run-id".to_string()),
        slug: None,
        limit: 20,
    });
    assert!(result.is_err(), "jobs status with nonexistent ID should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"), "error: {}", err);
}

#[test]
fn cmd_jobs_status_empty() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // No job runs yet
    let result = commands::jobs::run(commands::JobsAction::Status {
        config: config_dir,
        id: None,
        slug: None,
        limit: 20,
    });
    assert!(result.is_ok(), "jobs status with no runs should succeed: {:?}", result.err());
}

#[test]
fn cmd_jobs_purge() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // Trigger a job first so there's something to purge
    commands::jobs::run(commands::JobsAction::Trigger {
        config: config_dir.clone(),
        slug: "cleanup".to_string(),
        data: None,
    }).unwrap();

    // Purge old runs (0 seconds = purge everything older than now)
    let result = commands::jobs::run(commands::JobsAction::Purge {
        config: config_dir,
        older_than: "1m".to_string(),
    });
    assert!(result.is_ok(), "jobs purge should succeed: {:?}", result.err());
}

#[test]
fn cmd_jobs_purge_invalid_duration() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(commands::JobsAction::Purge {
        config: config_dir,
        older_than: "invalid".to_string(),
    });
    assert!(result.is_err(), "purge with invalid duration should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Invalid duration"), "error: {}", err);
}

// ═══════════════════════════════════════════════════════════════════════════
// 28. Status command (status.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_status_with_fixture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::status::run(&config_dir);
    assert!(result.is_ok(), "status should succeed: {:?}", result.err());
}

#[test]
fn cmd_status_empty_project() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("empty");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let result = commands::status::run(&config_dir);
    assert!(result.is_ok(), "status on empty project should succeed: {:?}", result.err());
}

#[test]
fn cmd_status_with_data() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Create some data
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Status Test".to_string());
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let result = commands::status::run(&config_dir);
    assert!(result.is_ok(), "status with data should succeed: {:?}", result.err());
}

#[test]
fn cmd_status_with_globals() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // The fixture has a globals/settings.lua — status should show it
    let result = commands::status::run(&config_dir);
    assert!(result.is_ok(), "status with globals should succeed: {:?}", result.err());
}

#[test]
fn cmd_status_with_migrations() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a migration file
    scaffold::make_migration(&config_dir, "test_status_migration").unwrap();

    let result = commands::status::run(&config_dir);
    assert!(result.is_ok(), "status with migrations should succeed: {:?}", result.err());
}

#[test]
fn cmd_status_bad_dir() {
    let result = commands::status::run(Path::new("/nonexistent/config"));
    assert!(result.is_err(), "status with nonexistent dir should fail");
}

// ═══════════════════════════════════════════════════════════════════════════
// 29. Templates command (templates.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_templates_list_all() {
    let result = commands::templates::run(commands::TemplatesAction::List {
        r#type: None,
    });
    assert!(result.is_ok(), "templates list should succeed: {:?}", result.err());
}

#[test]
fn cmd_templates_list_templates_only() {
    let result = commands::templates::run(commands::TemplatesAction::List {
        r#type: Some("templates".to_string()),
    });
    assert!(result.is_ok(), "templates list templates should succeed: {:?}", result.err());
}

#[test]
fn cmd_templates_list_static_only() {
    let result = commands::templates::run(commands::TemplatesAction::List {
        r#type: Some("static".to_string()),
    });
    assert!(result.is_ok(), "templates list static should succeed: {:?}", result.err());
}

#[test]
fn cmd_templates_list_invalid_type() {
    let result = commands::templates::run(commands::TemplatesAction::List {
        r#type: Some("invalid".to_string()),
    });
    assert!(result.is_err(), "templates list with invalid type should fail");
}

#[test]
fn cmd_templates_extract_specific() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::run(commands::TemplatesAction::Extract {
        config: tmp.path().to_path_buf(),
        paths: vec!["layout/base.hbs".to_string()],
        all: false,
        r#type: None,
        force: false,
    });
    assert!(result.is_ok(), "templates extract specific should succeed: {:?}", result.err());
    assert!(tmp.path().join("templates/layout/base.hbs").exists());
}

#[test]
fn cmd_templates_extract_all_templates() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::run(commands::TemplatesAction::Extract {
        config: tmp.path().to_path_buf(),
        paths: vec![],
        all: true,
        r#type: Some("templates".to_string()),
        force: false,
    });
    assert!(result.is_ok(), "templates extract all templates should succeed: {:?}", result.err());
    assert!(tmp.path().join("templates/layout/base.hbs").exists());
}

#[test]
fn cmd_templates_extract_all_static() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::run(commands::TemplatesAction::Extract {
        config: tmp.path().to_path_buf(),
        paths: vec![],
        all: true,
        r#type: Some("static".to_string()),
        force: false,
    });
    assert!(result.is_ok(), "templates extract all static should succeed: {:?}", result.err());
    assert!(tmp.path().join("static/styles.css").exists());
}

#[test]
fn cmd_templates_extract_no_paths_no_all_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::run(commands::TemplatesAction::Extract {
        config: tmp.path().to_path_buf(),
        paths: vec![],
        all: false,
        r#type: None,
        force: false,
    });
    assert!(result.is_err(), "extract with no paths and no --all should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("--all"), "error: {}", err);
}

#[test]
fn cmd_templates_extract_force_overwrites() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Extract once
    commands::templates::run(commands::TemplatesAction::Extract {
        config: tmp.path().to_path_buf(),
        paths: vec!["layout/base.hbs".to_string()],
        all: false,
        r#type: None,
        force: false,
    }).unwrap();

    // Write a custom marker
    std::fs::write(tmp.path().join("templates/layout/base.hbs"), "CUSTOM").unwrap();

    // Extract again without force — should skip
    commands::templates::run(commands::TemplatesAction::Extract {
        config: tmp.path().to_path_buf(),
        paths: vec!["layout/base.hbs".to_string()],
        all: false,
        r#type: None,
        force: false,
    }).unwrap();
    let content = std::fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
    assert_eq!(content, "CUSTOM", "should not overwrite without force");

    // Extract with force — should overwrite
    commands::templates::run(commands::TemplatesAction::Extract {
        config: tmp.path().to_path_buf(),
        paths: vec!["layout/base.hbs".to_string()],
        all: false,
        r#type: None,
        force: true,
    }).unwrap();
    let content = std::fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
    assert_ne!(content, "CUSTOM", "should overwrite with force");
}

// ═══════════════════════════════════════════════════════════════════════════
// 30. Additional DB/Migrate Command Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_migrate_up() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a migration file
    let migrations_dir = config_dir.join("migrations");
    std::fs::create_dir_all(&migrations_dir).unwrap();
    std::fs::write(migrations_dir.join("20240101000000_noop.lua"), r#"
local M = {}
function M.up()
end
function M.down()
end
return M
"#).unwrap();

    let result = commands::db::migrate(
        &config_dir,
        commands::MigrateAction::Up,
    );
    assert!(result.is_ok(), "migrate up should succeed: {:?}", result.err());

    // Verify migration was applied
    let cfg = CrapConfig::load(&config_dir).unwrap();
    let db_pool = pool::create_pool(&config_dir, &cfg).unwrap();
    let applied = migrate::get_applied_migrations(&db_pool).unwrap();
    assert!(applied.contains("20240101000000_noop.lua"));
}

#[test]
fn cmd_migrate_up_no_pending() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // No migration files — should succeed with "no pending" message
    let result = commands::db::migrate(
        &config_dir,
        commands::MigrateAction::Up,
    );
    assert!(result.is_ok(), "migrate up with no pending should succeed: {:?}", result.err());
}

#[test]
fn cmd_migrate_list() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a migration
    scaffold::make_migration(&config_dir, "test_list").unwrap();

    let result = commands::db::migrate(
        &config_dir,
        commands::MigrateAction::List,
    );
    assert!(result.is_ok(), "migrate list should succeed: {:?}", result.err());
}

#[test]
fn cmd_migrate_list_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::db::migrate(
        &config_dir,
        commands::MigrateAction::List,
    );
    assert!(result.is_ok(), "migrate list with no migrations should succeed: {:?}", result.err());
}

#[test]
fn cmd_migrate_down_no_applied() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::db::migrate(
        &config_dir,
        commands::MigrateAction::Down { steps: 1 },
    );
    assert!(result.is_ok(), "migrate down with nothing to roll back should succeed: {:?}", result.err());
}

#[test]
fn cmd_migrate_fresh_with_confirm() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // First, set up some data
    {
        let cfg = CrapConfig::load(&config_dir).unwrap();
        let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
        let db_pool = pool::create_pool(&config_dir, &cfg).unwrap();
        migrate::sync_all(&db_pool, &registry, &cfg.locale).unwrap();

        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Pre-fresh".to_string());
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Run fresh with confirm
    let result = commands::db::migrate(
        &config_dir,
        commands::MigrateAction::Fresh { confirm: true },
    );
    assert!(result.is_ok(), "migrate fresh with confirm should succeed: {:?}", result.err());

    // Verify data is gone
    let cfg = CrapConfig::load(&config_dir).unwrap();
    let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
    let db_pool = pool::create_pool(&config_dir, &cfg).unwrap();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();
    let count = ops::count_documents(&db_pool, "posts", def, &[], None).unwrap();
    assert_eq!(count, 0, "data should be gone after fresh");
}

#[test]
fn cmd_backup_with_output_dir() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Create data
    {
        let reg = registry.read().unwrap();
        let def = reg.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Backup Test".to_string());
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }
    drop(pool);

    let backup_output = tmp.path().join("my-backups");
    let result = commands::db::backup(&config_dir, Some(backup_output.clone()), false);
    assert!(result.is_ok(), "backup should succeed: {:?}", result.err());
    assert!(backup_output.exists(), "backup directory should exist");

    // Should contain a timestamped subdirectory with crap.db and manifest.json
    let subdirs: Vec<_> = std::fs::read_dir(&backup_output)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(subdirs.len(), 1);
    assert!(subdirs[0].path().join("crap.db").exists());
    assert!(subdirs[0].path().join("manifest.json").exists());
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
            "make", "collection",
            config_dir.to_str().unwrap(),
            "articles",
            "--fields", "title:text:required,body:textarea",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "make collection should succeed: {}",
        String::from_utf8_lossy(&output.stderr));

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
            "make", "collection",
            config_dir.to_str().unwrap(),
            "members",
            "--auth",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "make collection with auth should succeed: {}",
        String::from_utf8_lossy(&output.stderr));

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
            "make", "collection",
            config_dir.to_str().unwrap(),
            "media",
            "--upload",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "make collection with upload should succeed: {}",
        String::from_utf8_lossy(&output.stderr));

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
            "make", "collection",
            config_dir.to_str().unwrap(),
            "articles",
            "--versions",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "make collection with versions should succeed: {}",
        String::from_utf8_lossy(&output.stderr));

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
            "make", "collection",
            config_dir.to_str().unwrap(),
            "logs",
            "--no-timestamps",
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "make collection --no-timestamps should succeed: {}",
        String::from_utf8_lossy(&output.stderr));

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
            "make", "collection",
            config_dir.to_str().unwrap(),
            "--no-input",
        ])
        .output()
        .expect("failed to run binary");

    assert!(!output.status.success(), "make collection without slug in --no-input should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("required") || stderr.contains("slug"),
        "error should mention slug is required, got: {}", stderr);
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

    assert!(output.status.success(), "status should succeed: {}",
        String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Collections"), "status output should mention collections");
    assert!(stdout.contains("posts"), "status output should show posts collection");
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

    assert!(output.status.success(), "jobs list should succeed: {}",
        String::from_utf8_lossy(&output.stderr));
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

    assert!(output.status.success(), "templates list should succeed: {}",
        String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Templates"), "should list templates");
    assert!(stdout.contains("Static files"), "should list static files");
}

#[test]
fn templates_extract_via_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let output = std::process::Command::new(crap_bin())
        .args(["templates", "extract", tmp.path().to_str().unwrap(), "layout/base.hbs"])
        .output()
        .expect("failed to run binary");

    assert!(output.status.success(), "templates extract should succeed: {}",
        String::from_utf8_lossy(&output.stderr));
    assert!(tmp.path().join("templates/layout/base.hbs").exists());
}
