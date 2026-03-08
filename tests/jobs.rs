use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::job::{JobStatus};
use crap_cms::db::{migrate, pool, query};
use crap_cms::db::query::jobs as job_query;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/job_tests")
}

fn setup() -> (tempfile::TempDir, crap_cms::db::DbPool, crap_cms::core::SharedRegistry, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("Failed to init Lua");

    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut pool_config = CrapConfig::default();
    pool_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &pool_config).expect("Failed to create pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("Failed to sync schema");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("Failed to create HookRunner");
    (tmp, db_pool, registry, runner)
}

// ── Job Definition Loading ──────────────────────────────────────────────

#[test]
fn job_definitions_loaded_from_lua() {
    let (_tmp, _pool, registry, _runner) = setup();
    let reg = registry.read().unwrap();

    assert!(reg.get_job("test_create_post").is_some(), "test_create_post job should be defined");
    assert!(reg.get_job("test_failing_job").is_some(), "test_failing_job should be defined");
    assert!(reg.get_job("test_echo_job").is_some(), "test_echo_job should be defined");

    let def = reg.get_job("test_create_post").unwrap();
    assert_eq!(def.handler, "jobs.test_job.create_post");
    assert_eq!(def.retries, 1);
    assert_eq!(def.timeout, 30);

    let fail_def = reg.get_job("test_failing_job").unwrap();
    assert_eq!(fail_def.retries, 2);
}

// ── Job Queuing (DB operations) ─────────────────────────────────────────

#[test]
fn insert_job_creates_pending_row() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run = job_query::insert_job(&conn, "test_echo_job", "{\"key\":\"value\"}", "manual", 1, "default")
        .expect("insert_job");

    assert!(!run.id.is_empty());
    assert_eq!(run.slug, "test_echo_job");
    assert_eq!(run.status, JobStatus::Pending);
    assert_eq!(run.data, "{\"key\":\"value\"}");
    assert_eq!(run.attempt, 0);
    assert_eq!(run.max_attempts, 1);
}

#[test]
fn claim_pending_jobs_marks_running() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    // Insert two pending jobs with different slugs (each has default concurrency=1)
    job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    job_query::insert_job(&conn, "test_create_post", "{}", "manual", 1, "default").unwrap();

    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    let claimed = job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();

    assert_eq!(claimed.len(), 2, "Should claim both pending jobs (different slugs)");
    for job in &claimed {
        assert_eq!(job.status, JobStatus::Running);
    }
}

#[test]
fn complete_job_sets_completed_status() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run = job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    let claimed = job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();
    assert_eq!(claimed.len(), 1);

    job_query::complete_job(&conn, &run.id, Some("{\"done\":true}")).unwrap();

    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Completed);
    assert_eq!(fetched.result.as_deref(), Some("{\"done\":true}"));
    assert!(fetched.completed_at.is_some());
}

#[test]
fn fail_job_with_retry_resets_to_pending() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    // max_attempts = 3 (retries=2 means 3 total attempts)
    let run = job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 3, "default").unwrap();
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();

    // Fail with should_retry = true (attempt < max_attempts)
    job_query::fail_job(&conn, &run.id, "test error", true).unwrap();

    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Pending, "Should reset to pending for retry");
    assert_eq!(fetched.attempt, 1, "Attempt should be incremented");
    assert_eq!(fetched.error.as_deref(), Some("test error"));
}

#[test]
fn fail_job_no_retry_stays_failed() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run = job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 1, "default").unwrap();
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();

    // Fail with should_retry = false
    job_query::fail_job(&conn, &run.id, "permanent failure", false).unwrap();

    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Failed);
    assert_eq!(fetched.error.as_deref(), Some("permanent failure"));
}

#[test]
fn list_job_runs_filters() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    job_query::insert_job(&conn, "test_failing_job", "{}", "cron", 1, "default").unwrap();

    // Filter by slug
    let echo_runs = job_query::list_job_runs(&conn, Some("test_echo_job"), None, 50, 0).unwrap();
    assert_eq!(echo_runs.len(), 1);
    assert_eq!(echo_runs[0].slug, "test_echo_job");

    // Filter by status
    let pending_runs = job_query::list_job_runs(&conn, None, Some("pending"), 50, 0).unwrap();
    assert_eq!(pending_runs.len(), 2);

    // No filter
    let all = job_query::list_job_runs(&conn, None, None, 50, 0).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn count_running_jobs() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    // Use different slugs to avoid per-job concurrency=1 limiting claims
    job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    job_query::insert_job(&conn, "test_create_post", "{}", "manual", 1, "default").unwrap();

    assert_eq!(job_query::count_running(&conn, None).unwrap(), 0);

    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();

    assert_eq!(job_query::count_running(&conn, None).unwrap(), 2);
    assert_eq!(job_query::count_running(&conn, Some("test_echo_job")).unwrap(), 1);
    assert_eq!(job_query::count_running(&conn, Some("test_create_post")).unwrap(), 1);
    assert_eq!(job_query::count_running(&conn, Some("nonexistent")).unwrap(), 0);
}

#[test]
fn purge_old_jobs() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run = job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();
    job_query::complete_job(&conn, &run.id, None).unwrap();

    // Backdate created_at so the purge threshold catches it
    conn.execute(
        "UPDATE _crap_jobs SET created_at = datetime('now', '-3600 seconds') WHERE id = ?1",
        [&run.id],
    ).unwrap();

    // Purge with 60 seconds threshold should purge the backdated completed job
    let purged = job_query::purge_old_jobs(&conn, 60).unwrap();
    assert_eq!(purged, 1);

    // Verify it's gone
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap();
    assert!(fetched.is_none(), "Purged job should not be found");
}

// ── Job Execution (HookRunner) ──────────────────────────────────────────

#[test]
fn execute_echo_job_via_hook_runner() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner.run_job_handler(
        "jobs.test_job.echo",
        "test_echo_job",
        "{\"hello\":\"world\"}",
        1,
        1,
        &conn,
    ).expect("run_job_handler");

    assert!(result.is_some(), "Echo job should return a result");
    let json: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
    assert_eq!(json.get("hello").unwrap().as_str().unwrap(), "world");
}

#[test]
fn execute_job_that_creates_document() {
    let (_tmp, pool, registry, runner) = setup();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    runner.run_job_handler(
        "jobs.test_job.create_post",
        "test_create_post",
        "{\"title\":\"From Job\"}",
        1,
        1,
        &tx,
    ).expect("run_job_handler");

    tx.commit().expect("Commit");

    // Verify the post was created
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    let conn2 = pool.get().expect("DB connection");
    let docs = query::find(&conn2, "posts", &def, &query::FindQuery::default(), None)
        .expect("find posts");
    assert!(!docs.is_empty(), "Job should have created a post");
    let doc = &docs[0];
    assert_eq!(doc.fields.get("title").and_then(|v| v.as_str()), Some("From Job"));
}

#[test]
fn execute_failing_job_returns_error() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner.run_job_handler(
        "jobs.test_job.fail",
        "test_failing_job",
        "{}",
        1,
        3,
        &conn,
    );

    assert!(result.is_err(), "Failing job should return an error");
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("intentional failure"), "Error should contain the failure message: {}", err_msg);
}

// ── Stale Job Detection ─────────────────────────────────────────────────

#[test]
fn find_stale_jobs_detects_running() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run = job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();

    // Manually backdate the heartbeat so it appears stale
    conn.execute(
        "UPDATE _crap_jobs SET heartbeat_at = datetime('now', '-600 seconds') WHERE id = ?1",
        [&run.id],
    ).unwrap();

    // With threshold 60 seconds, the backdated heartbeat should be detected as stale
    let stale = job_query::find_stale_jobs(&conn, 60).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].status, JobStatus::Running);
}

#[test]
fn mark_stale_changes_status() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run = job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();

    job_query::mark_stale(&conn, &run.id, "server restarted").unwrap();

    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Stale);
    assert_eq!(fetched.error.as_deref(), Some("server restarted"));
}
