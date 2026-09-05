//! Binary invocation tests for operational CLI commands:
//! typegen, proto, db cleanup, jobs, images, migrate, blueprint, make hook/job.

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

/// Run the binary with the given args and return the raw Output.
fn run(args: &[&str]) -> std::process::Output {
    std::process::Command::new(crap_bin())
        .args(args)
        .output()
        .expect("failed to run binary")
}

/// Run the binary, assert success, and return stdout as a String.
fn run_ok(args: &[&str]) -> String {
    let output = run(args);
    assert!(
        output.status.success(),
        "Command {:?} failed.\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
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

/// Run the binary with extra env vars, assert success, return stdout.
fn run_ok_env(args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new(crap_bin());
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to run binary");
    assert!(
        output.status.success(),
        "Command {:?} failed.\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Run the binary with `CRAP_CONFIG_DIR` and extra env vars, assert success, return stdout.
fn run_ok_in_env(config_dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new(crap_bin());
    cmd.env("CRAP_CONFIG_DIR", config_dir).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to run binary");
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

/// Setup with an additional photos collection (upload-enabled) for images tests.
fn setup_with_photos() -> (tempfile::TempDir, PathBuf) {
    let (tmp, config_dir) = setup();
    let photos_lua = r#"crap.collections.define("photos", {
    labels = { singular = "Photo", plural = "Photos" },
    upload = true,
    fields = {
        { name = "title", type = "text" },
    },
})"#;
    std::fs::write(config_dir.join("collections/photos.lua"), photos_lua).unwrap();
    (tmp, config_dir)
}

/// Setup with a cleanup job definition for jobs tests.
fn setup_with_job() -> (tempfile::TempDir, PathBuf) {
    let (tmp, config_dir) = setup();
    std::fs::create_dir_all(config_dir.join("jobs")).unwrap();
    // The handler must actually exist — startup validation resolves job
    // refs like any other hook ref.
    let job_lua = r#"local M = {}

M.run = crap.any.job_handler(function(_context)
end)

crap.jobs.define("cleanup", {
    handler = "jobs.cleanup.run",
    schedule = "0 3 * * *",
    queue = "maintenance",
})

return M"#;
    std::fs::write(config_dir.join("jobs/cleanup.lua"), job_lua).unwrap();
    (tmp, config_dir)
}

// ═══════════════════════════════════════════════════════════════════════════
// Typegen
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn typegen_lua() {
    let (_tmp, config_dir) = setup();

    run_ok_in(&config_dir, &["typegen", "lua"]);

    let types_dir = config_dir.join("types");
    assert!(
        types_dir.join("crap.lua").exists(),
        "typegen lua should write types/crap.lua"
    );
    assert!(
        types_dir.join("hooks.lua").exists(),
        "typegen lua should write types/hooks.lua"
    );
}

#[test]
fn typegen_client_all_languages() {
    let (_tmp, config_dir) = setup();

    run_ok_in(&config_dir, &["typegen", "client", "--lang", "ts,go,py,rs"]);

    let types_dir = config_dir.join("types");
    for ext in &["ts", "go", "py", "rs"] {
        let expected = types_dir.join(format!("client.{ext}"));
        assert!(
            expected.exists(),
            "typegen client should write types/client.{ext}, missing: {}",
            expected.display()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Proto
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn proto_to_stdout() {
    let stdout = run_ok(&["proto"]);

    assert!(
        stdout.contains("service ContentAPI"),
        "proto output should contain service definition, got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

#[test]
fn proto_to_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_file = tmp.path().join("content.proto");

    run_ok(&["proto", "-o", out_file.to_str().unwrap()]);

    let content = std::fs::read_to_string(&out_file).unwrap();
    assert!(
        content.contains("service ContentAPI"),
        "proto file should contain service definition"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// DB Cleanup
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn db_cleanup_dry_run() {
    let (_tmp, config_dir) = setup();

    // Run cleanup in dry-run mode (no --confirm)
    let stdout = run_ok_in(&config_dir, &["db", "cleanup"]);

    // Should succeed and show status (either clean or listing orphans)
    assert!(
        stdout.contains("orphan") || stdout.contains("clean") || stdout.contains("column"),
        "cleanup output should report orphan status, got: {stdout}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Jobs
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn jobs_trigger_and_status() {
    let (_tmp, config_dir) = setup_with_job();

    // Trigger the job
    let stdout = run_ok_in(&config_dir, &["jobs", "trigger", "cleanup"]);
    assert!(
        stdout.contains("Queued job"),
        "should confirm job queued, got: {stdout}"
    );

    // Check status
    let stdout = run_ok_in(&config_dir, &["jobs", "status", "--slug", "cleanup"]);
    assert!(
        stdout.contains("cleanup") && stdout.contains("run"),
        "status should show the triggered job run, got: {stdout}"
    );
}

#[test]
fn jobs_cancel() {
    let (_tmp, config_dir) = setup_with_job();

    // Trigger initializes the DB via init_stack()
    run_ok_in(&config_dir, &["jobs", "trigger", "cleanup"]);

    // Cancel it
    let stdout = run_ok_in(&config_dir, &["jobs", "cancel", "--slug", "cleanup"]);
    assert!(
        stdout.contains("Cancelled"),
        "should confirm cancellation, got: {stdout}"
    );
}

#[test]
fn jobs_healthcheck() {
    let (_tmp, config_dir) = setup_with_job();

    // The fixture defines a *scheduled* job that has never completed a run,
    // which classifies as `warning` — and the exit code follows the status
    // (0 healthy / 2 warning / 1 unhealthy) so CI can gate on it.
    let output = run_in(&config_dir, &["jobs", "healthcheck"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(2),
        "scheduled-but-never-ran job must exit 2 (warning); stdout: {stdout}"
    );
    assert!(
        stdout.contains("Job system health"),
        "should show health status, got: {stdout}"
    );
    assert!(
        stdout.contains("Defined:"),
        "should show defined jobs count, got: {stdout}"
    );
    assert!(
        stdout.contains("warning"),
        "status line should say warning, got: {stdout}"
    );
}

#[test]
fn jobs_purge() {
    let (_tmp, config_dir) = setup_with_job();

    // Initialize DB (jobs purge skips sync_all, so trigger list first)
    run_ok_in(&config_dir, &["jobs", "list"]);

    let stdout = run_ok_in(&config_dir, &["jobs", "purge", "--older-than", "0s"]);
    assert!(
        stdout.contains("Purged"),
        "should confirm purge, got: {stdout}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Images
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn images_list_empty() {
    let (_tmp, config_dir) = setup_with_photos();

    // Initialize DB (images commands skip sync_all)
    run_ok_in(&config_dir, &["status"]);

    let stdout = run_ok_in(&config_dir, &["images", "list"]);
    assert!(
        stdout.contains("No image-convert jobs found"),
        "should show empty queue, got: {stdout}"
    );
}

#[test]
fn images_stats_empty() {
    let (_tmp, config_dir) = setup_with_photos();

    // Initialize DB (images commands skip sync_all)
    run_ok_in(&config_dir, &["status"]);

    let stdout = run_ok_in(&config_dir, &["images", "stats"]);
    assert!(
        stdout.contains("Image processing queue"),
        "should show queue stats header, got: {stdout}"
    );
    assert!(
        stdout.contains("Total:"),
        "should show total count, got: {stdout}"
    );
}

#[test]
fn images_purge_empty() {
    let (_tmp, config_dir) = setup_with_photos();

    // Initialize DB (images commands skip sync_all)
    run_ok_in(&config_dir, &["status"]);

    let stdout = run_ok_in(&config_dir, &["images", "purge", "--older-than", "0s"]);
    assert!(
        stdout.contains("Purged"),
        "should confirm purge, got: {stdout}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Migrate (up, down, list, fresh)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn migrate_up_and_list() {
    let (_tmp, config_dir) = setup();

    // Create a migration
    run_ok_in(&config_dir, &["migrate", "create", "add_categories"]);

    // Run migrate up
    let stdout = run_ok_in(&config_dir, &["migrate", "up"]);
    assert!(
        stdout.contains("applied")
            || stdout.contains("Applied")
            || stdout.contains("Schema sync complete"),
        "migrate up should show progress, got: {stdout}"
    );

    // List migrations
    let stdout = run_ok_in(&config_dir, &["migrate", "list"]);
    assert!(
        stdout.contains("add_categories"),
        "migrate list should show the migration, got: {stdout}"
    );
    assert!(
        stdout.contains("applied"),
        "migration should show as applied, got: {stdout}"
    );
}

#[test]
fn migrate_down() {
    let (_tmp, config_dir) = setup();

    // Create a migration and run up
    run_ok_in(&config_dir, &["migrate", "create", "add_tags"]);
    run_ok_in(&config_dir, &["migrate", "up"]);

    // Run migrate down
    let stdout = run_ok_in(&config_dir, &["migrate", "down"]);
    assert!(
        stdout.contains("Rolled back") || stdout.contains("rolled back"),
        "migrate down should confirm rollback, got: {stdout}"
    );

    // Verify via list
    let stdout = run_ok_in(&config_dir, &["migrate", "list"]);
    assert!(
        stdout.contains("pending"),
        "migration should show as pending after rollback, got: {stdout}"
    );
}

#[test]
fn migrate_fresh_confirm() {
    let (_tmp, config_dir) = setup();

    // Create a user first (to verify fresh wipes data)
    run_ok_in(
        &config_dir,
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
    );

    // Run migrate fresh with -y
    let stdout = run_ok_in(&config_dir, &["migrate", "fresh", "-y"]);
    assert!(
        stdout.contains("Fresh migration complete"),
        "should confirm fresh migration, got: {stdout}"
    );

    // Verify data is wiped
    let list_out = run_ok_in(&config_dir, &["user", "list"]);
    assert!(
        list_out.contains("No users"),
        "user list should be empty after fresh, got: {list_out}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Blueprint
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn blueprint_save_list_remove() {
    let (_tmp, config_dir) = setup();
    let bp_home = _tmp.path().join("xdg_config");

    let env = [("XDG_CONFIG_HOME", bp_home.to_str().unwrap())];

    // Save
    let stdout = run_ok_in_env(&config_dir, &["blueprint", "save", "test-bp"], &env);
    assert!(
        stdout.contains("Saved blueprint 'test-bp'"),
        "should confirm save, got: {stdout}"
    );

    // List
    let stdout = run_ok_env(&["blueprint", "list"], &env);
    assert!(
        stdout.contains("test-bp"),
        "list should include the blueprint, got: {stdout}"
    );

    // Remove
    let stdout = run_ok_env(&["blueprint", "remove", "test-bp"], &env);
    assert!(
        stdout.contains("Removed blueprint 'test-bp'"),
        "should confirm removal, got: {stdout}"
    );

    // List should be empty now
    let stdout = run_ok_env(&["blueprint", "list"], &env);
    assert!(
        stdout.contains("No blueprints"),
        "list should be empty after remove, got: {stdout}"
    );
}

#[test]
fn blueprint_use() {
    let (_tmp, config_dir) = setup();
    let bp_home = _tmp.path().join("xdg_config");
    let target_dir = _tmp.path().join("new_project");

    let env = [("XDG_CONFIG_HOME", bp_home.to_str().unwrap())];

    // Save blueprint
    run_ok_in_env(&config_dir, &["blueprint", "save", "test-bp"], &env);

    // Use blueprint
    let stdout = run_ok_env(
        &["blueprint", "use", "test-bp", target_dir.to_str().unwrap()],
        &env,
    );
    assert!(
        stdout.contains("Created project from blueprint 'test-bp'"),
        "should confirm project creation, got: {stdout}"
    );

    assert!(
        target_dir.join("crap.toml").exists(),
        "target dir should have crap.toml"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Make Hook / Make Job
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn make_hook_via_binary() {
    let (_tmp, config_dir) = setup();

    let stdout = run_ok_in(
        &config_dir,
        &[
            "make",
            "hook",
            "auto_slug",
            "-t",
            "collection",
            "-c",
            "posts",
            "-l",
            "before_change",
        ],
    );

    assert!(
        stdout.contains("Created"),
        "should confirm hook creation, got: {stdout}"
    );
    assert!(
        stdout.contains("Hook ref:"),
        "should show hook ref, got: {stdout}"
    );

    // Verify file exists
    let hook_path = config_dir.join("hooks/posts/auto_slug.lua");
    assert!(
        hook_path.exists(),
        "hook file should be created at {hook_path:?}"
    );

    let content = std::fs::read_to_string(&hook_path).unwrap();
    assert!(
        content.contains("function"),
        "hook file should contain a function"
    );
}

#[test]
fn make_job_via_binary() {
    let (_tmp, config_dir) = setup();

    let stdout = run_ok_in(
        &config_dir,
        &[
            "make",
            "job",
            "cleanup",
            "--schedule",
            "0 3 * * *",
            "--queue",
            "maintenance",
        ],
    );

    assert!(
        stdout.contains("Created"),
        "should confirm job creation, got: {stdout}"
    );
    assert!(
        stdout.contains("Handler ref:"),
        "should show handler ref, got: {stdout}"
    );

    // Verify file exists
    let job_path = config_dir.join("jobs/cleanup.lua");
    assert!(
        job_path.exists(),
        "job file should be created at {job_path:?}"
    );

    let content = std::fs::read_to_string(&job_path).unwrap();
    assert!(
        content.contains("crap.jobs.define"),
        "job file should contain crap.jobs.define"
    );
    assert!(
        content.contains("function"),
        "job file should contain a function"
    );
}
