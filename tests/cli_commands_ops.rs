//! CLI command function tests for crap-cms.
//!
//! Tests for command library functions (sections 18-30):
//! direct Rust calls without invoking the binary.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use serde_json::json;
use std::path::{Path, PathBuf};

use crap_cms::commands;
use crap_cms::config::CrapConfig;
use crap_cms::core::DocumentFields;
use crap_cms::core::auth;
use crap_cms::db::{DbPool, migrate, ops, pool, query};
use crap_cms::hooks;
use crap_cms::scaffold;

// ── Helpers ──────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("cli_tests")
}

/// Copy fixture dir to a temp dir, init Lua, create pool, sync schema.
/// Returns (`TempDir`, `DbPool`, Arc<Registry>).
fn full_setup() -> (
    tempfile::TempDir,
    DbPool,
    std::sync::Arc<crap_cms::core::Registry>,
) {
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

/// Create a user in an auth collection via `query::create` + `update_password`.
fn create_user(
    pool: &DbPool,
    def: &crap_cms::core::CollectionDefinition,
    email: &str,
    password: &str,
    extra_fields: &[(&str, &str)],
) -> crap_cms::core::Document {
    let mut data = DocumentFields::new();
    data.insert("email".to_string(), json!(email.to_string()));
    for (k, v) in extra_fields {
        data.insert(k.to_string(), json!(v.to_string()));
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

/// Set up a fixture dir that also includes a job definition.
fn full_setup_with_jobs() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    std::sync::Arc<crap_cms::core::Registry>,
) {
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
    )
    .unwrap();

    let cfg = CrapConfig::load(&config_dir).expect("load config");
    let registry = hooks::init_lua(&config_dir, &cfg).expect("init lua");
    let db_pool = pool::create_pool(&config_dir, &cfg).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &cfg.locale).expect("sync schema");

    (tmp, db_pool, registry)
}

// ═══════════════════════════════════════════════════════════════════════════
// 18. Command Export/Import Functions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_user_lock_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let def = registry.get_collection("users").unwrap().clone();

    let doc = create_user(
        &pool,
        &def,
        "lockme@example.com",
        "pw",
        &[("name", "Lock Me")],
    );

    // Verify not locked initially
    let conn = pool.get().unwrap();
    assert!(!query::is_locked(&conn, "users", &doc.id).unwrap());
    drop(conn);

    // Lock via command
    commands::user_lock(
        &pool,
        &registry,
        "users",
        Some("lockme@example.com".to_string()),
        None,
    )
    .unwrap();

    // Verify locked
    let conn = pool.get().unwrap();
    assert!(query::is_locked(&conn, "users", &doc.id).unwrap());
}

#[test]
fn cmd_user_lock_by_id() {
    let (_tmp, pool, registry) = full_setup();
    let def = registry.get_collection("users").unwrap().clone();

    let doc = create_user(
        &pool,
        &def,
        "lockid@example.com",
        "pw",
        &[("name", "Lock ID")],
    );

    // Lock via ID
    commands::user_lock(&pool, &registry, "users", None, Some(doc.id.to_string())).unwrap();

    let conn = pool.get().unwrap();
    assert!(query::is_locked(&conn, "users", &doc.id).unwrap());
}

#[test]
fn cmd_user_unlock_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let def = registry.get_collection("users").unwrap().clone();

    let doc = create_user(
        &pool,
        &def,
        "unlockme@example.com",
        "pw",
        &[("name", "Unlock Me")],
    );

    // Lock first
    let conn = pool.get().unwrap();
    query::lock_user(&conn, "users", &doc.id).unwrap();
    assert!(query::is_locked(&conn, "users", &doc.id).unwrap());
    drop(conn);

    // Unlock via command
    commands::user_unlock(
        &pool,
        &registry,
        "users",
        Some("unlockme@example.com".to_string()),
        None,
    )
    .unwrap();

    let conn = pool.get().unwrap();
    assert!(!query::is_locked(&conn, "users", &doc.id).unwrap());
}

#[test]
fn cmd_user_delete_with_confirm_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let def = registry.get_collection("users").unwrap().clone();

    let doc = create_user(
        &pool,
        &def,
        "deleteme@example.com",
        "pw",
        &[("name", "Delete Me")],
    );
    let id = doc.id.clone();

    // Delete with confirm=true (skips interactive prompt)
    commands::user_delete(commands::UserDeleteParams {
        pool: &pool,
        registry: &registry,
        locale: &crap_cms::config::LocaleConfig::default(),
        collection: "users",
        email: Some("deleteme@example.com".to_string()),
        id: None,
        confirm: true, // skip confirmation
    })
    .unwrap();

    // Verify deleted
    let def = registry.get_collection("users").unwrap();
    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", def, &id, None).unwrap();
    assert!(found.is_none(), "user should be deleted");
}

#[test]
fn cmd_user_delete_with_confirm_by_id() {
    let (_tmp, pool, registry) = full_setup();
    let def = registry.get_collection("users").unwrap().clone();

    let doc = create_user(
        &pool,
        &def,
        "delbyid@example.com",
        "pw",
        &[("name", "Delete By ID")],
    );
    let id = doc.id.to_string();

    // Delete by ID with confirm=true
    commands::user_delete(commands::UserDeleteParams {
        pool: &pool,
        registry: &registry,
        locale: &crap_cms::config::LocaleConfig::default(),
        collection: "users",
        email: None,
        id: Some(id.clone()),
        confirm: true,
    })
    .unwrap();

    let def = registry.get_collection("users").unwrap();
    let conn = pool.get().unwrap();
    let found = query::find_by_id(&conn, "users", def, &id, None).unwrap();
    assert!(found.is_none(), "user should be deleted");
}

#[test]
fn cmd_user_delete_nonexistent_email_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user_delete(commands::UserDeleteParams {
        pool: &pool,
        registry: &registry,
        locale: &crap_cms::config::LocaleConfig::default(),
        collection: "users",
        email: Some("nonexistent@example.com".to_string()),
        id: None,
        confirm: true,
    });
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("No user found"), "error: {err}");
}

#[test]
fn cmd_user_change_password_by_email() {
    let (_tmp, pool, registry) = full_setup();
    let def = registry.get_collection("users").unwrap().clone();

    let doc = create_user(
        &pool,
        &def,
        "chpw@example.com",
        "oldpw",
        &[("name", "ChPW User")],
    );

    // Change password via command (programmatic, not interactive)
    commands::user_change_password(commands::UserChangePasswordParams {
        pool: &pool,
        registry: &registry,
        collection: "users",
        email: Some("chpw@example.com".to_string()),
        id: None,
        password: Some("newpw123".to_string()),
        password_policy: &crap_cms::config::PasswordPolicy::default(),
    })
    .unwrap();

    // Verify new password works
    let conn = pool.get().unwrap();
    let hash = query::get_password_hash(&conn, "users", &doc.id)
        .unwrap()
        .unwrap();
    assert!(auth::verify_password("newpw123", hash.as_ref()).unwrap());
    assert!(!auth::verify_password("oldpw", hash.as_ref()).unwrap());
}

#[test]
fn cmd_user_change_password_by_id() {
    let (_tmp, pool, registry) = full_setup();
    let def = registry.get_collection("users").unwrap().clone();

    let doc = create_user(
        &pool,
        &def,
        "chpwid@example.com",
        "oldpw",
        &[("name", "ChPW ID")],
    );

    commands::user_change_password(commands::UserChangePasswordParams {
        pool: &pool,
        registry: &registry,
        collection: "users",
        email: None,
        id: Some(doc.id.to_string()),
        password: Some("newpw456".to_string()),
        password_policy: &crap_cms::config::PasswordPolicy::default(),
    })
    .unwrap();

    let conn = pool.get().unwrap();
    let hash = query::get_password_hash(&conn, "users", &doc.id)
        .unwrap()
        .unwrap();
    assert!(auth::verify_password("newpw456", hash.as_ref()).unwrap());
}

#[test]
fn cmd_user_change_password_nonexistent_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user_change_password(commands::UserChangePasswordParams {
        pool: &pool,
        registry: &registry,
        collection: "users",
        email: Some("noone@example.com".to_string()),
        id: None,
        password: Some("newpw".to_string()),
        password_policy: &crap_cms::config::PasswordPolicy::default(),
    });
    assert!(result.is_err());
}

#[test]
fn cmd_user_lock_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user_lock(
        &pool,
        &registry,
        "posts",
        Some("anyone@example.com".to_string()),
        None,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {err}");
}

#[test]
fn cmd_user_unlock_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user_unlock(
        &pool,
        &registry,
        "posts",
        Some("anyone@example.com".to_string()),
        None,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {err}");
}

#[test]
fn cmd_user_delete_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user_delete(commands::UserDeleteParams {
        pool: &pool,
        registry: &registry,
        locale: &crap_cms::config::LocaleConfig::default(),
        collection: "posts",
        email: Some("anyone@example.com".to_string()),
        id: None,
        confirm: true,
    });
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {err}");
}

#[test]
fn cmd_user_change_password_non_auth_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user_change_password(commands::UserChangePasswordParams {
        pool: &pool,
        registry: &registry,
        collection: "posts",
        email: Some("anyone@example.com".to_string()),
        id: None,
        password: Some("newpw".to_string()),
        password_policy: &crap_cms::config::PasswordPolicy::default(),
    });
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not an auth collection"), "error: {err}");
}

#[test]
fn cmd_user_create_missing_collection_errors() {
    let (_tmp, pool, registry) = full_setup();

    let result = commands::user_create(commands::UserCreateParams {
        pool: &pool,
        registry: &registry,
        collection: "nonexistent",
        email: Some("test@example.com".to_string()),
        password: Some("pw".to_string()),
        fields: vec![],
        password_policy: &crap_cms::config::PasswordPolicy::default(),
        locale: &crap_cms::config::LocaleConfig::default(),
    });
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"), "error: {err}");
}

// ═══════════════════════════════════════════════════════════════════════════
// 27. Jobs command functions (jobs.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_jobs_list() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(&config_dir, commands::JobsAction::List);
    assert!(
        result.is_ok(),
        "jobs list should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_jobs_list_empty() {
    // Use fixture without jobs
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::jobs::run(&config_dir, commands::JobsAction::List);
    assert!(
        result.is_ok(),
        "jobs list with no jobs should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_jobs_trigger_and_status() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // Trigger a job
    commands::jobs::run(
        &config_dir,
        commands::JobsAction::Trigger {
            slug: "cleanup".to_string(),
            data: Some(r#"{"key": "value"}"#.to_string()),
            priority: None,
        },
    )
    .unwrap();

    // Check status (list all runs)
    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Status {
            id: None,
            slug: Some("cleanup".to_string()),
            limit: 10,
        },
    );
    assert!(
        result.is_ok(),
        "jobs status should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_jobs_trigger_nonexistent_errors() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Trigger {
            slug: "nonexistent_job".to_string(),
            data: None,
            priority: None,
        },
    );
    assert!(result.is_err(), "triggering nonexistent job should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not defined"), "error: {err}");
}

#[test]
fn cmd_jobs_trigger_invalid_json_errors() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Trigger {
            slug: "cleanup".to_string(),
            data: Some("not valid json {{{".to_string()),
            priority: None,
        },
    );
    assert!(result.is_err(), "triggering with invalid JSON should fail");
}

#[test]
fn cmd_jobs_status_single_run() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // Trigger a job first
    commands::jobs::run(
        &config_dir,
        commands::JobsAction::Trigger {
            slug: "cleanup".to_string(),
            data: None,
            priority: None,
        },
    )
    .unwrap();

    // Get the run ID from the database
    let cfg = CrapConfig::load(&config_dir).unwrap();
    let db_pool = pool::create_pool(&config_dir, &cfg).unwrap();
    let conn = db_pool.get().unwrap();
    let runs =
        crap_cms::db::query::jobs::list_job_runs(&conn, Some("cleanup"), None, 10, 0).unwrap();
    assert!(!runs.is_empty());
    let run_id = runs[0].id.clone();
    drop(conn);
    drop(db_pool);

    // Show single run by ID
    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Status {
            id: Some(run_id),
            slug: None,
            limit: 20,
        },
    );
    assert!(
        result.is_ok(),
        "jobs status by ID should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_jobs_status_not_found() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Status {
            id: Some("nonexistent-run-id".to_string()),
            slug: None,
            limit: 20,
        },
    );
    assert!(
        result.is_err(),
        "jobs status with nonexistent ID should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"), "error: {err}");
}

#[test]
fn cmd_jobs_status_empty() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // No job runs yet
    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Status {
            id: None,
            slug: None,
            limit: 20,
        },
    );
    assert!(
        result.is_ok(),
        "jobs status with no runs should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_jobs_purge() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    // Trigger a job first so there's something to purge
    commands::jobs::run(
        &config_dir,
        commands::JobsAction::Trigger {
            slug: "cleanup".to_string(),
            data: None,
            priority: None,
        },
    )
    .unwrap();

    // Purge old runs (0 seconds = purge everything older than now)
    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Purge {
            older_than: "1m".to_string(),
        },
    );
    assert!(
        result.is_ok(),
        "jobs purge should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_jobs_purge_invalid_duration() {
    let (tmp, _pool, _registry) = full_setup_with_jobs();
    let config_dir = tmp.path().join("config");

    let result = commands::jobs::run(
        &config_dir,
        commands::JobsAction::Purge {
            older_than: "invalid".to_string(),
        },
    );
    assert!(result.is_err(), "purge with invalid duration should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Invalid duration"), "error: {err}");
}

// ═══════════════════════════════════════════════════════════════════════════
// 28. Status command (status.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_status_with_fixture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::status::run(&config_dir, false);
    assert!(result.is_ok(), "status should succeed: {:?}", result.err());
}

#[test]
fn cmd_status_empty_project() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("empty");
    scaffold::init(Some(config_dir.clone()), &scaffold::InitOptions::default()).unwrap();

    let result = commands::status::run(&config_dir, false);
    assert!(
        result.is_ok(),
        "status on empty project should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_status_with_data() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Create some data
    {
        let def = registry.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Status Test".to_string()));
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let result = commands::status::run(&config_dir, false);
    assert!(
        result.is_ok(),
        "status with data should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_status_with_globals() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // The fixture has a globals/settings.lua — status should show it
    let result = commands::status::run(&config_dir, false);
    assert!(
        result.is_ok(),
        "status with globals should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_status_with_migrations() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a migration file
    scaffold::make_migration(&config_dir, "test_status_migration").unwrap();

    let result = commands::status::run(&config_dir, false);
    assert!(
        result.is_ok(),
        "status with migrations should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_status_bad_dir() {
    let result = commands::status::run(Path::new("/nonexistent/config"), false);
    assert!(result.is_err(), "status with nonexistent dir should fail");
}

// ═══════════════════════════════════════════════════════════════════════════
// 29. Templates command (templates.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn cmd_templates_list_all() {
    let result = commands::templates::list(None, false);
    assert!(
        result.is_ok(),
        "templates list should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_templates_list_templates_only() {
    let result = commands::templates::list(Some("templates"), false);
    assert!(
        result.is_ok(),
        "templates list templates should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_templates_list_static_only() {
    let result = commands::templates::list(Some("static"), false);
    assert!(
        result.is_ok(),
        "templates list static should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_templates_list_invalid_type() {
    let result = commands::templates::list(Some("invalid"), false);
    assert!(
        result.is_err(),
        "templates list with invalid type should fail"
    );
}

#[test]
fn cmd_templates_list_verbose() {
    let result = commands::templates::list(None, true);
    assert!(
        result.is_ok(),
        "templates list --verbose should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_templates_extract_specific() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::extract(
        tmp.path(),
        &["layout/base.hbs".to_string()],
        false,
        None,
        false,
    );
    assert!(
        result.is_ok(),
        "templates extract specific should succeed: {:?}",
        result.err()
    );
    assert!(tmp.path().join("templates/layout/base.hbs").exists());
}

#[test]
fn cmd_templates_extract_all_templates() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::extract(tmp.path(), &[], true, Some("templates"), false);
    assert!(
        result.is_ok(),
        "templates extract all templates should succeed: {:?}",
        result.err()
    );
    assert!(tmp.path().join("templates/layout/base.hbs").exists());
}

#[test]
fn cmd_templates_extract_all_static() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::extract(tmp.path(), &[], true, Some("static"), false);
    assert!(
        result.is_ok(),
        "templates extract all static should succeed: {:?}",
        result.err()
    );
    assert!(tmp.path().join("static/styles/main.css").exists());
}

#[test]
fn cmd_templates_extract_no_paths_no_all_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let result = commands::templates::extract(tmp.path(), &[], false, None, false);
    assert!(
        result.is_err(),
        "extract with no paths and no --all should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(err.contains("--all"), "error: {err}");
}

#[test]
fn cmd_templates_extract_force_overwrites() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Extract once
    commands::templates::extract(
        tmp.path(),
        &["layout/base.hbs".to_string()],
        false,
        None,
        false,
    )
    .unwrap();

    // Write a custom marker
    std::fs::write(tmp.path().join("templates/layout/base.hbs"), "CUSTOM").unwrap();

    // Extract again without force — should skip
    commands::templates::extract(
        tmp.path(),
        &["layout/base.hbs".to_string()],
        false,
        None,
        false,
    )
    .unwrap();
    let content = std::fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
    assert_eq!(content, "CUSTOM", "should not overwrite without force");

    // Extract with force — should overwrite
    commands::templates::extract(
        tmp.path(),
        &["layout/base.hbs".to_string()],
        false,
        None,
        true,
    )
    .unwrap();
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
    std::fs::write(
        migrations_dir.join("20240101000000_noop.lua"),
        r"
local M = {}
function M.up()
end
function M.down()
end
return M
",
    )
    .unwrap();

    let result = commands::db::migrate(&config_dir, &commands::MigrateAction::Up);
    assert!(
        result.is_ok(),
        "migrate up should succeed: {:?}",
        result.err()
    );

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
    let result = commands::db::migrate(&config_dir, &commands::MigrateAction::Up);
    assert!(
        result.is_ok(),
        "migrate up with no pending should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_migrate_list() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    // Create a migration
    scaffold::make_migration(&config_dir, "test_list").unwrap();

    let result = commands::db::migrate(&config_dir, &commands::MigrateAction::List);
    assert!(
        result.is_ok(),
        "migrate list should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_migrate_list_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::db::migrate(&config_dir, &commands::MigrateAction::List);
    assert!(
        result.is_ok(),
        "migrate list with no migrations should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cmd_migrate_down_no_applied() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);

    let result = commands::db::migrate(&config_dir, &commands::MigrateAction::Down { steps: 1 });
    assert!(
        result.is_ok(),
        "migrate down with nothing to roll back should succeed: {:?}",
        result.err()
    );
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

        let def = registry.get_collection("posts").unwrap();
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Pre-fresh".to_string()));
        query::create(&tx, "posts", def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Run fresh with confirm
    let result = commands::db::migrate(
        &config_dir,
        &commands::MigrateAction::Fresh { confirm: true },
    );
    assert!(
        result.is_ok(),
        "migrate fresh with confirm should succeed: {:?}",
        result.err()
    );

    // Verify data is gone
    let cfg = CrapConfig::load(&config_dir).unwrap();
    let registry = hooks::init_lua(&config_dir, &cfg).unwrap();
    let db_pool = pool::create_pool(&config_dir, &cfg).unwrap();
    let def = registry.get_collection("posts").unwrap();
    let count = ops::count_documents(&db_pool, "posts", def, &[], None).unwrap();
    assert_eq!(count, 0, "data should be gone after fresh");
}

#[test]
fn cmd_backup_with_output_dir() {
    let (tmp, pool, registry) = full_setup();
    let config_dir = tmp.path().join("config");

    // Create data
    {
        let def = registry.get_collection("posts").unwrap();
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Backup Test".to_string()));
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
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(subdirs.len(), 1);
    assert!(subdirs[0].path().join("crap.db").exists());
    assert!(subdirs[0].path().join("manifest.json").exists());
}
