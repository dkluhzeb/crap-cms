//! CLI integration tests for crap-cms.
//!
//! Two layers:
//! 1. Library function tests — direct Rust calls with temp dirs.
//! 2. Binary invocation tests — `std::process::Command` for clap parsing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crap_cms::config::CrapConfig;
use crap_cms::core::auth;
use crap_cms::db::{DbConnection, DbPool, DbValue, migrate, pool, query};
use crap_cms::hooks;
use crap_cms::scaffold;
use serde_json::json;

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
    for cmd in &[
        "serve",
        "status",
        "user",
        "make",
        "init",
        "export",
        "import",
        "backup",
        "restore",
        "migrate",
        "typegen",
        "proto",
        "blueprint",
    ] {
        assert!(
            stdout.contains(cmd),
            "help should list '{}' command, got:\n{}",
            cmd,
            stdout
        );
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
    assert!(
        stdout.contains("crap-cms"),
        "version should contain binary name"
    );
}

#[test]
fn user_subcommand_help() {
    let output = std::process::Command::new(crap_bin())
        .args(["user", "--help"])
        .output()
        .expect("failed to run binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    for sub in &[
        "create",
        "list",
        "delete",
        "lock",
        "unlock",
        "change-password",
    ] {
        assert!(
            stdout.contains(sub),
            "user help should list '{}', got:\n{}",
            sub,
            stdout
        );
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
        assert!(
            stdout.contains(sub),
            "make help should list '{}', got:\n{}",
            sub,
            stdout
        );
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
        assert!(
            stdout.contains(sub),
            "migrate help should list '{}', got:\n{}",
            sub,
            stdout
        );
    }
}

#[test]
fn migrate_create_via_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let output = std::process::Command::new(crap_bin())
        .env("CRAP_CONFIG_DIR", config_dir.to_str().unwrap())
        .args(["migrate", "create", "add_tags"])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "migrate create should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

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
    assert!(
        content.contains("crap"),
        "types/crap.lua should contain crap API definitions"
    );
}

#[test]
fn init_refuses_existing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("existing");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("crap.toml"), "# existing").unwrap();

    let result = scaffold::init(Some(target), &scaffold::InitOptions::default());
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("refusing to overwrite")
    );
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
    assert!(
        cfg.is_ok(),
        "scaffolded crap.toml should load: {:?}",
        cfg.err()
    );
}

#[test]
fn init_lua_loadable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("lua-test");
    scaffold::init(Some(target.clone()), &scaffold::InitOptions::default()).unwrap();

    let cfg = CrapConfig::load(&target).unwrap();
    let result = hooks::init_lua(&target, &cfg);
    assert!(
        result.is_ok(),
        "scaffolded init.lua should load: {:?}",
        result.err()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Make Collection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn make_collection_default() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(
        tmp.path(),
        "posts",
        None,
        &scaffold::CollectionOptions::default(),
    )
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
    assert!(content.contains("crap.collections.define(\"posts\""));
    assert!(content.contains("timestamps = true"));
    assert!(content.contains("crap.fields.text({"));
    assert!(content.contains("name = \"title\""));
}

#[test]
fn make_collection_fields_shorthand() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fields = scaffold::parse_fields_shorthand("title:text:required,body:textarea").unwrap();
    scaffold::make_collection(
        tmp.path(),
        "articles",
        Some(&fields),
        &scaffold::CollectionOptions::default(),
    )
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("collections/articles.lua")).unwrap();
    assert!(content.contains("crap.fields.text({"));
    assert!(content.contains("name = \"title\""));
    assert!(content.contains("crap.fields.textarea({"));
    assert!(content.contains("name = \"body\""));
    assert!(content.contains("required = true"));
}

#[test]
fn make_collection_no_timestamps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = scaffold::CollectionOptions {
        no_timestamps: true,
        ..scaffold::CollectionOptions::default()
    };
    scaffold::make_collection(tmp.path(), "logs", None, &opts).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("collections/logs.lua")).unwrap();
    assert!(content.contains("timestamps = false"));
}

#[test]
fn make_collection_refuses_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(
        tmp.path(),
        "posts",
        None,
        &scaffold::CollectionOptions::default(),
    )
    .unwrap();
    let result = scaffold::make_collection(
        tmp.path(),
        "posts",
        None,
        &scaffold::CollectionOptions::default(),
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--force"));
}

#[test]
fn make_collection_force_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_collection(
        tmp.path(),
        "posts",
        None,
        &scaffold::CollectionOptions::default(),
    )
    .unwrap();
    let opts = scaffold::CollectionOptions {
        force: true,
        ..scaffold::CollectionOptions::default()
    };
    assert!(scaffold::make_collection(tmp.path(), "posts", None, &opts).is_ok());
}

#[test]
fn make_collection_invalid_slug() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = scaffold::CollectionOptions::default();
    assert!(scaffold::make_collection(tmp.path(), "Posts", None, &opts).is_err());
    assert!(scaffold::make_collection(tmp.path(), "my-slug", None, &opts).is_err());
    assert!(scaffold::make_collection(tmp.path(), "_private", None, &opts).is_err());
    assert!(scaffold::make_collection(tmp.path(), "", None, &opts).is_err());
}

#[test]
fn make_collection_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let fields = scaffold::parse_fields_shorthand("title:text:required,body:richtext").unwrap();
    scaffold::make_collection(
        &config_dir,
        "articles",
        Some(&fields),
        &scaffold::CollectionOptions::default(),
    )
    .unwrap();

    let cfg = CrapConfig::load(&config_dir).unwrap();
    let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
    let reg = registry.read().unwrap();
    assert!(
        reg.get_collection("articles").is_some(),
        "articles should be in registry"
    );
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
    scaffold::make_global(tmp.path(), "site_settings", None, false).unwrap();

    let path = tmp.path().join("globals/site_settings.lua");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("crap.globals.define(\"site_settings\""));
}

#[test]
fn make_global_refuses_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold::make_global(tmp.path(), "settings", None, false).unwrap();
    let result = scaffold::make_global(tmp.path(), "settings", None, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--force"));
}

#[test]
fn make_global_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("project");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    scaffold::make_global(&config_dir, "navigation", None, false).unwrap();

    let cfg = CrapConfig::load(&config_dir).unwrap();
    let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
    let reg = registry.read().unwrap();
    assert!(
        reg.get_global("navigation").is_some(),
        "navigation should be in registry"
    );
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
    scaffold::MakeHookOptions {
        config_dir,
        name,
        hook_type,
        collection,
        position,
        field,
        force,
        condition_field: None,
        is_global: false,
    }
}

#[test]
fn make_hook_creates_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(),
        "auto_slug",
        scaffold::HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
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
        tmp.path(),
        "auto_slug",
        scaffold::HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
    );
    scaffold::make_hook(&opts).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("hooks/posts/auto_slug.lua")).unwrap();
    assert!(content.contains("crap.hook.Posts"));
    assert!(content.contains("before_change hook for posts"));
    assert!(content.contains("return function(context)"));
    assert!(content.contains("return context"));
}

#[test]
fn make_hook_field_hook() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(),
        "normalize",
        scaffold::HookType::Field,
        "posts",
        "before_validate",
        Some("title"),
        false,
    );
    scaffold::make_hook(&opts).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("hooks/posts/normalize.lua")).unwrap();
    assert!(
        content.contains("crap.field_hook.Posts"),
        "should use typed field hook context"
    );
    assert!(content.contains("return function(value, context)"));
    assert!(content.contains("return value"));
}

#[test]
fn make_hook_access_hook() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(),
        "admin_only",
        scaffold::HookType::Access,
        "posts",
        "read",
        None,
        false,
    );
    scaffold::make_hook(&opts).unwrap();

    let content = std::fs::read_to_string(tmp.path().join("access/admin_only.lua")).unwrap();
    assert!(content.contains("crap.AccessContext"));
    assert!(content.contains("return true"));
}

#[test]
fn make_hook_refuses_overwrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(),
        "auto_slug",
        scaffold::HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
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
        tmp.path(),
        "auto_slug",
        scaffold::HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
    );
    scaffold::make_hook(&opts).unwrap();
    let opts_force = hook_opts(
        tmp.path(),
        "auto_slug",
        scaffold::HookType::Collection,
        "posts",
        "before_change",
        None,
        true,
    );
    assert!(scaffold::make_hook(&opts_force).is_ok());
}

#[test]
fn make_hook_invalid_position() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = hook_opts(
        tmp.path(),
        "bad",
        scaffold::HookType::Collection,
        "posts",
        "not_a_real_position",
        None,
        false,
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
        tmp.path(),
        "auto_slug",
        scaffold::HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
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
    assert!(
        filename.ends_with("_add_categories.lua"),
        "got: {}",
        filename
    );
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
    assert!(
        users_def.is_auth_collection(),
        "users should be an auth collection"
    );

    let posts_def = reg.get_collection("posts").unwrap();
    assert!(
        !posts_def.is_auth_collection(),
        "posts should NOT be an auth collection"
    );
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

    let doc = create_user(
        &pool,
        &def,
        "test@example.com",
        "secret123",
        &[("name", "Test User")],
    );

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

    let doc = create_user(
        &pool,
        &def,
        "admin@example.com",
        "pw123",
        &[("name", "Admin"), ("role", "admin")],
    );

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

    let doc = create_user(
        &pool,
        &def,
        "pw@example.com",
        "mypassword",
        &[("name", "PW User")],
    );

    let conn = pool.get().unwrap();
    let hash = query::get_password_hash(&conn, "users", &doc.id)
        .unwrap()
        .expect("password hash should exist");
    assert!(
        hash.as_ref().starts_with("$argon2"),
        "should be argon2 hash"
    );
    assert!(auth::verify_password("mypassword", hash.as_ref()).unwrap());
    assert!(!auth::verify_password("wrongpassword", hash.as_ref()).unwrap());
}

#[test]
fn user_find_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    create_user(
        &pool,
        &def,
        "lookup@example.com",
        "pw",
        &[("name", "Lookup")],
    );

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

    let doc = create_user(
        &pool,
        &def,
        "delete@example.com",
        "pw",
        &[("name", "Delete Me")],
    );
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

    let doc = create_user(
        &pool,
        &def,
        "lock@example.com",
        "pw",
        &[("name", "Lockable")],
    );

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

    let doc = create_user(
        &pool,
        &def,
        "chpw@example.com",
        "oldpassword",
        &[("name", "ChPW")],
    );

    let conn = pool.get().unwrap();
    query::update_password(&conn, "users", &doc.id, "newpassword").unwrap();

    let hash = query::get_password_hash(&conn, "users", &doc.id)
        .unwrap()
        .unwrap();
    assert!(auth::verify_password("newpassword", hash.as_ref()).unwrap());
    assert!(!auth::verify_password("oldpassword", hash.as_ref()).unwrap());
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
        let docs_json: Vec<serde_json::Value> = docs
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        collections_data.insert(slug.to_string(), serde_json::Value::Array(docs_json));
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
    assert!(
        reg.get_collection("nonexistent").is_none(),
        "nonexistent collection should not be found"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. Import
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn import_basic() {
    let (_tmp, pool, registry) = full_setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap();

    let import_json = json!({
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
            if field.has_parent_column()
                && let Some(val) = doc_obj.get(&field.name)
                && let Some(s) = val.as_str()
            {
                parent_cols.push(field.name.clone());
                parent_vals.push(s.to_string());
            }
        }
        let placeholders: Vec<String> = (0..parent_cols.len())
            .map(|i| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "INSERT OR REPLACE INTO \"{}\" ({}) VALUES ({})",
            "posts",
            parent_cols
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", "),
            placeholders.join(", ")
        );
        let db_params: Vec<DbValue> = parent_vals
            .iter()
            .map(|v| DbValue::Text(v.clone()))
            .collect();
        tx.execute(&sql, &db_params).unwrap();
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

    let import_json = json!({
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
            &format!(
                "INSERT OR REPLACE INTO \"{}\" (id, title) VALUES (?1, ?2)",
                slug
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(title.to_string()),
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    // Verify only posts imported
    let conn = pool.get().unwrap();
    let posts = query::find(&conn, "posts", def, &query::FindQuery::default(), None).unwrap();
    assert_eq!(posts.len(), 1);

    let users_def = reg.get_collection("users").unwrap();
    let users = query::find(
        &conn,
        "users",
        users_def,
        &query::FindQuery::default(),
        None,
    )
    .unwrap();
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
        &[
            DbValue::Text("custom-id-123".into()),
            DbValue::Text("Custom ID Post".into()),
        ],
    )
    .unwrap();
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
        &[
            DbValue::Text("ts-post".into()),
            DbValue::Text("Timestamped".into()),
            DbValue::Text("2024-01-01 00:00:00".into()),
            DbValue::Text("2024-06-15 12:30:00".into()),
        ],
    )
    .unwrap();
    tx.commit().unwrap();

    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "posts", def, "ts-post", None)
        .unwrap()
        .unwrap();
    assert_eq!(
        found.created_at.as_deref(),
        Some("2024-01-01T00:00:00.000Z")
    );
    assert_eq!(
        found.updated_at.as_deref(),
        Some("2024-06-15T12:30:00.000Z")
    );
}

#[test]
fn import_unknown_collection_fails() {
    let (_tmp, _pool, registry) = full_setup();
    let reg = registry.read().unwrap();

    let import_json = json!({
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
            assert!(
                found.is_none(),
                "nonexistent collection should not be found"
            );
        }
    }
}
