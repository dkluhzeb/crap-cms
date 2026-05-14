//! Binary invocation tests for data-oriented CLI commands:
//! export, import, backup, restore, and user management.

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

use std::path::{Path, PathBuf};

// ── Helpers ──────────────────────────────────────────────────────────────

fn crap_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_crap-cms"))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("cli_tests")
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

/// Run the binary with `CRAP_CONFIG_DIR` set, return raw Output.
fn run_in(config_dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(crap_bin())
        .env("CRAP_CONFIG_DIR", config_dir)
        .args(args)
        .output()
        .expect("failed to run binary")
}

/// Run the binary with `CRAP_CONFIG_DIR` set, assert success, return stdout.
fn run_ok_in(config_dir: &Path, args: &[&str]) -> String {
    let output = run_in(config_dir, args);
    assert!(
        output.status.success(),
        "Command {:?} failed.\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Copy fixture to tempdir and return (`TempDir`, `config_dir` path).
fn setup() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("config");
    copy_dir(&fixture_dir(), &config_dir);
    (tmp, config_dir)
}

/// Create a test user in the given config dir via the binary.
/// Returns stdout from user create.
fn create_test_user(config_dir: &Path) -> String {
    run_ok_in(
        config_dir,
        &[
            "user",
            "create",
            "-e",
            "test@example.com",
            "-p",
            "password123",
            "--field",
            "name=Test User",
        ],
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// Export
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn export_to_file() {
    let (tmp, config_dir) = setup();
    let out_file = tmp.path().join("export.json");

    run_ok_in(&config_dir, &["export", "-o", out_file.to_str().unwrap()]);

    let content = std::fs::read_to_string(&out_file).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(
        json.get("crap_version").is_some(),
        "should have crap_version key"
    );
    assert!(
        json.get("collections").is_some(),
        "should have collections key"
    );
}

#[test]
fn export_single_collection() {
    let (tmp, config_dir) = setup();
    let out_file = tmp.path().join("export.json");

    run_ok_in(
        &config_dir,
        &["export", "-c", "posts", "-o", out_file.to_str().unwrap()],
    );

    let content = std::fs::read_to_string(&out_file).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    let collections = json.get("collections").unwrap().as_object().unwrap();
    assert!(collections.contains_key("posts"), "should have posts");
    assert_eq!(collections.len(), 1, "should only export one collection");
}

#[test]
fn export_nonexistent_collection_fails() {
    let (tmp, config_dir) = setup();
    let out_file = tmp.path().join("export.json");

    let output = run_in(
        &config_dir,
        &[
            "export",
            "-c",
            "nonexistent",
            "-o",
            out_file.to_str().unwrap(),
        ],
    );

    assert!(
        !output.status.success(),
        "exporting nonexistent collection should fail"
    );
}

#[test]
fn import_roundtrip() {
    let (tmp, config_dir) = setup();
    let export_file = tmp.path().join("export.json");

    // Create a user
    create_test_user(&config_dir);

    // Export all data
    run_ok_in(
        &config_dir,
        &["export", "-o", export_file.to_str().unwrap()],
    );

    // Delete the user
    run_ok_in(
        &config_dir,
        &["user", "delete", "-e", "test@example.com", "-y"],
    );

    // Verify user is gone
    let list_out = run_ok_in(&config_dir, &["user", "list"]);
    assert!(
        list_out.contains("No users"),
        "user should be deleted, got: {list_out}"
    );

    // Import the exported data
    run_ok_in(&config_dir, &["import", export_file.to_str().unwrap()]);

    // Verify user is back
    let list_out = run_ok_in(&config_dir, &["user", "list"]);
    assert!(
        list_out.contains("test@example.com"),
        "user should be restored after import, got: {list_out}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Backup / Restore
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn backup_creates_directory() {
    let (tmp, config_dir) = setup();
    let backup_dir = tmp.path().join("backups");

    // Initialize the database by running status (triggers schema sync)
    run_ok_in(&config_dir, &["status"]);

    let stdout = run_ok_in(&config_dir, &["backup", "-o", backup_dir.to_str().unwrap()]);

    assert!(
        stdout.contains("Backup complete"),
        "should print backup complete, got: {stdout}"
    );

    // Find the backup subdirectory (named backup-<timestamp>)
    let entries: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .collect();
    assert_eq!(entries.len(), 1, "should create one backup subdirectory");

    let backup_subdir = entries[0].path();
    assert!(
        backup_subdir.join("manifest.json").exists(),
        "backup should contain manifest.json"
    );
    assert!(
        backup_subdir.join("crap.db").exists(),
        "backup should contain crap.db"
    );
}

#[test]
fn backup_restore_roundtrip() {
    let (tmp, config_dir) = setup();
    let backup_dir = tmp.path().join("backups");

    // Create a user
    create_test_user(&config_dir);

    // Backup
    run_ok_in(&config_dir, &["backup", "-o", backup_dir.to_str().unwrap()]);

    // Find the backup subdirectory
    let entries: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .collect();
    let backup_subdir = entries[0].path();

    // Delete the user
    run_ok_in(
        &config_dir,
        &["user", "delete", "-e", "test@example.com", "-y"],
    );

    // Restore
    run_ok_in(
        &config_dir,
        &["restore", backup_subdir.to_str().unwrap(), "-y"],
    );

    // Verify user is back
    let list_out = run_ok_in(&config_dir, &["user", "list"]);
    assert!(
        list_out.contains("test@example.com"),
        "user should exist after restore, got: {list_out}"
    );
}

#[test]
fn restore_requires_confirm() {
    let (tmp, config_dir) = setup();
    let backup_dir = tmp.path().join("backups");

    // Initialize the database
    run_ok_in(&config_dir, &["status"]);

    // Create a backup first
    run_ok_in(&config_dir, &["backup", "-o", backup_dir.to_str().unwrap()]);

    let entries: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .collect();
    let backup_subdir = entries[0].path();

    // Restore without -y should fail
    let output = run_in(&config_dir, &["restore", backup_subdir.to_str().unwrap()]);

    assert!(!output.status.success(), "restore without -y should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("confirm") || stderr.contains("-y"),
        "error should mention confirm flag, got: {stderr}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// User Management
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn user_create_and_list() {
    let (_tmp, config_dir) = setup();

    let stdout = create_test_user(&config_dir);
    assert!(
        stdout.contains("Created user"),
        "should confirm user creation, got: {stdout}"
    );

    let list_out = run_ok_in(&config_dir, &["user", "list"]);
    assert!(
        list_out.contains("test@example.com"),
        "user list should contain the email, got: {list_out}"
    );
    assert!(
        list_out.contains("1 user(s)"),
        "should show user count, got: {list_out}"
    );
}

#[test]
fn user_info_by_email() {
    let (_tmp, config_dir) = setup();
    create_test_user(&config_dir);

    let stdout = run_ok_in(&config_dir, &["user", "info", "-e", "test@example.com"]);

    assert!(
        stdout.contains("test@example.com"),
        "info should show email, got: {stdout}"
    );
    assert!(
        stdout.contains("users"),
        "info should show collection, got: {stdout}"
    );
    assert!(
        stdout.contains("Test User"),
        "info should show name field, got: {stdout}"
    );
}

#[test]
fn user_lock_and_unlock() {
    let (_tmp, config_dir) = setup();
    create_test_user(&config_dir);

    // Lock
    let stdout = run_ok_in(&config_dir, &["user", "lock", "-e", "test@example.com"]);
    assert!(
        stdout.contains("Locked user"),
        "should confirm lock, got: {stdout}"
    );

    // Verify locked via info
    let info = run_ok_in(&config_dir, &["user", "info", "-e", "test@example.com"]);
    assert!(
        info.contains("Locked:") && info.contains("yes"),
        "should show locked status, got: {info}"
    );

    // Unlock
    let stdout = run_ok_in(&config_dir, &["user", "unlock", "-e", "test@example.com"]);
    assert!(
        stdout.contains("Unlocked user"),
        "should confirm unlock, got: {stdout}"
    );

    // Verify unlocked via info
    let info = run_ok_in(&config_dir, &["user", "info", "-e", "test@example.com"]);
    assert!(
        info.contains("Locked:") && info.contains("no"),
        "should show unlocked status, got: {info}"
    );
}

#[test]
fn user_verify_and_unverify() {
    let (_tmp, config_dir) = setup();

    // Overwrite users.lua with verify_email enabled
    let users_lua = r#"crap.collections.define("users", {
    auth = {
        verify_email = true,
    },
    labels = {
        singular = "User",
        plural = "Users",
    },
    timestamps = true,
    admin = {
        use_as_title = "name",
    },
    fields = {
        {
            name = "name",
            type = "text",
            required = true,
        },
        {
            name = "role",
            type = "select",
            default_value = "editor",
            options = {
                { label = "Admin", value = "admin" },
                { label = "Editor", value = "editor" },
            },
        },
    },
})"#;
    std::fs::write(config_dir.join("collections/users.lua"), users_lua).unwrap();

    // Create a user
    create_test_user(&config_dir);

    // Verify
    let stdout = run_ok_in(&config_dir, &["user", "verify", "-e", "test@example.com"]);
    assert!(
        stdout.contains("Verified user"),
        "should confirm verify, got: {stdout}"
    );

    // Check via info
    let info = run_ok_in(&config_dir, &["user", "info", "-e", "test@example.com"]);
    assert!(
        info.contains("Verified:") && info.contains("yes"),
        "should show verified status, got: {info}"
    );

    // Unverify
    let stdout = run_ok_in(&config_dir, &["user", "unverify", "-e", "test@example.com"]);
    assert!(
        stdout.contains("Unverified user"),
        "should confirm unverify, got: {stdout}"
    );

    // Check via info
    let info = run_ok_in(&config_dir, &["user", "info", "-e", "test@example.com"]);
    assert!(
        info.contains("Verified:") && info.contains("no"),
        "should show unverified status, got: {info}"
    );
}

#[test]
fn user_delete_with_confirm() {
    let (_tmp, config_dir) = setup();
    create_test_user(&config_dir);

    let stdout = run_ok_in(
        &config_dir,
        &["user", "delete", "-e", "test@example.com", "-y"],
    );
    assert!(
        stdout.contains("Deleted user"),
        "should confirm deletion, got: {stdout}"
    );

    let list_out = run_ok_in(&config_dir, &["user", "list"]);
    assert!(
        list_out.contains("No users"),
        "user list should be empty after delete, got: {list_out}"
    );
}

#[test]
fn user_change_password() {
    let (_tmp, config_dir) = setup();
    create_test_user(&config_dir);

    let stdout = run_ok_in(
        &config_dir,
        &[
            "user",
            "change-password",
            "-e",
            "test@example.com",
            "-p",
            "newpassword456",
        ],
    );
    assert!(
        stdout.contains("Password changed"),
        "should confirm password change, got: {stdout}"
    );
}
