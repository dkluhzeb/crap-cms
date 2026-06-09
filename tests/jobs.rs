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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crap_cms::config::CrapConfig;
use crap_cms::core::HookRef;
use crap_cms::core::job::{JobRun, JobStatus};
use crap_cms::db::query::jobs as job_query;
use crap_cms::db::{DbConnection, DbValue, migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/job_tests")
}

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    std::sync::Arc<crap_cms::core::Registry>,
    HookRunner,
) {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).expect("Failed to init Lua");

    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &pool_config).expect("Failed to create pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("Failed to sync schema");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("Failed to create HookRunner");
    (tmp, db_pool, registry, runner)
}

// ── Job Definition Loading ──────────────────────────────────────────────

#[test]
fn job_definitions_loaded_from_lua() {
    let (_tmp, _pool, registry, _runner) = setup();

    assert!(
        registry.get_job("test_create_post").is_some(),
        "test_create_post job should be defined"
    );
    assert!(
        registry.get_job("test_failing_job").is_some(),
        "test_failing_job should be defined"
    );
    assert!(
        registry.get_job("test_echo_job").is_some(),
        "test_echo_job should be defined"
    );

    let def = registry.get_job("test_create_post").unwrap();
    assert_eq!(def.handler.reference(), "jobs.test_job.create_post");
    assert_eq!(def.retries, Some(1));
    assert_eq!(def.timeout, 30);

    let fail_def = registry.get_job("test_failing_job").unwrap();
    assert_eq!(fail_def.retries, Some(2));
}

// ── Job Queuing (DB operations) ─────────────────────────────────────────

#[test]
fn insert_job_creates_pending_row() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run = job_query::insert_job(
        &conn,
        "test_echo_job",
        "{\"key\":\"value\"}",
        "manual",
        1,
        "default",
        0,
    )
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
    job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    job_query::insert_job(&conn, "test_create_post", "{}", "manual", 1, "default", 0).unwrap();

    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();

    assert_eq!(
        claimed.len(),
        2,
        "Should claim both pending jobs (different slugs)"
    );
    for job in &claimed {
        assert_eq!(job.status, JobStatus::Running);
    }
}

#[test]
fn complete_job_sets_completed_status() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run =
        job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
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
    let run =
        job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 3, "default", 0).unwrap();
    let job_concurrency = HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();

    // Fail with should_retry = true (attempt < max_attempts)
    job_query::fail_job(&conn, &run.id, "test error", true, 1).unwrap();

    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(
        fetched.status,
        JobStatus::Pending,
        "Should reset to pending for retry"
    );
    assert_eq!(fetched.attempt, 1, "Attempt should be incremented");
    assert_eq!(fetched.error.as_deref(), Some("test error"));
}

#[test]
fn fail_job_no_retry_stays_failed() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run =
        job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 1, "default", 0).unwrap();
    let job_concurrency = HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();

    // Fail with should_retry = false
    job_query::fail_job(&conn, &run.id, "permanent failure", false, 1).unwrap();

    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Failed);
    assert_eq!(fetched.error.as_deref(), Some("permanent failure"));
}

#[test]
fn list_job_runs_filters() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    job_query::insert_job(&conn, "test_failing_job", "{}", "cron", 1, "default", 0).unwrap();

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
    job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    job_query::insert_job(&conn, "test_create_post", "{}", "manual", 1, "default", 0).unwrap();

    assert_eq!(job_query::count_running(&conn, None).unwrap(), 0);

    let job_concurrency = HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();

    assert_eq!(job_query::count_running(&conn, None).unwrap(), 2);
    assert_eq!(
        job_query::count_running(&conn, Some("test_echo_job")).unwrap(),
        1
    );
    assert_eq!(
        job_query::count_running(&conn, Some("test_create_post")).unwrap(),
        1
    );
    assert_eq!(
        job_query::count_running(&conn, Some("nonexistent")).unwrap(),
        0
    );
}

#[test]
fn purge_old_jobs() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run =
        job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    let job_concurrency = HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    job_query::complete_job(&conn, &run.id, None).unwrap();

    // Backdate created_at so the purge threshold catches it
    conn.execute(
        "UPDATE _crap_jobs SET created_at = datetime('now', '-3600 seconds') WHERE id = ?1",
        &[DbValue::Text(run.id.clone())],
    )
    .unwrap();

    // Purge with 60 seconds threshold should purge the backdated completed job
    let purged = job_query::purge_old_jobs(&conn, 60).unwrap();
    assert_eq!(purged, 1);

    // Verify it's gone
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap();
    assert!(fetched.is_none(), "Purged job should not be found");
}

// ── Job Execution (HookRunner) ──────────────────────────────────────────

/// Build a minimal `JobRun` for driving `run_job_handler` directly in tests.
fn job_run(slug: &str, data: &str, attempt: u32, max_attempts: u32) -> JobRun {
    JobRun::builder("test-run-id", slug)
        .data(data)
        .attempt(attempt)
        .max_attempts(max_attempts)
        .build()
}

#[test]
fn execute_echo_job_via_hook_runner() {
    let (_tmp, pool, _registry, runner) = setup();
    let result = runner
        .run_job_handler(
            &HookRef::new("jobs.test_job.echo"),
            &job_run("test_echo_job", "{\"hello\":\"world\"}", 1, 1),
            &pool,
        )
        .expect("run_job_handler");

    assert!(result.is_some(), "Echo job should return a result");
    let json: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
    assert_eq!(json.get("hello").unwrap().as_str().unwrap(), "world");
}

#[test]
fn execute_job_that_creates_document() {
    let (_tmp, pool, registry, runner) = setup();

    runner
        .run_job_handler(
            &HookRef::new("jobs.test_job.create_post"),
            &job_run("test_create_post", "{\"title\":\"From Job\"}", 1, 1),
            &pool,
        )
        .expect("run_job_handler");

    // Verify the post was created
    let def = registry.get_collection("posts").unwrap().clone();

    let conn2 = pool.get().expect("DB connection");
    let docs =
        query::find(&conn2, "posts", &def, &query::FindQuery::default(), None).expect("find posts");
    assert!(!docs.is_empty(), "Job should have created a post");
    let doc = &docs[0];
    assert_eq!(
        doc.fields.get("title").and_then(|v| v.as_str()),
        Some("From Job")
    );
}

/// Regression: the job handler context's `job` must expose the run id, queue,
/// priority, trigger source (`scheduled_by`), and `queued_at` — not just slug /
/// attempt / `max_attempts`.
#[test]
fn job_handler_receives_run_metadata() {
    let (_tmp, pool, _registry, runner) = setup();
    let jr = JobRun::builder("run-42", "meta-job")
        .queue("emails")
        .priority(7)
        .data("{}")
        .attempt(1)
        .max_attempts(3)
        .scheduled_by("cron")
        .unique_key("dedup-key-1")
        .created_at("2026-01-01T00:00:00Z")
        .build();

    let result = runner
        .run_job_handler(&HookRef::new("jobs.test_job.job_meta"), &jr, &pool)
        .expect("run_job_handler")
        .expect("handler returned a value");
    let json: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(json["id"], "run-42");
    assert_eq!(json["queue"], "emails");
    assert_eq!(json["priority"], 7);
    assert_eq!(
        json["scheduled_by"], "cron",
        "handler must see who triggered it"
    );
    assert_eq!(json["queued_at"], "2026-01-01T00:00:00Z");
    assert_eq!(
        json["unique_key"], "dedup-key-1",
        "handler must see the dedup unique_key"
    );
}

#[test]
fn execute_failing_job_returns_error() {
    let (_tmp, pool, _registry, runner) = setup();
    let result = runner.run_job_handler(
        &HookRef::new("jobs.test_job.fail"),
        &job_run("test_failing_job", "{}", 1, 3),
        &pool,
    );

    assert!(result.is_err(), "Failing job should return an error");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("intentional failure"),
        "Error should contain the failure message: {err_msg}"
    );
}

// ── Stale Job Detection ─────────────────────────────────────────────────

#[test]
fn find_stale_jobs_detects_running() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run =
        job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    let job_concurrency = HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();

    // Manually backdate the heartbeat so it appears stale
    conn.execute(
        "UPDATE _crap_jobs SET heartbeat_at = datetime('now', '-600 seconds') WHERE id = ?1",
        &[DbValue::Text(run.id.clone())],
    )
    .unwrap();

    // With threshold 60 seconds, the backdated heartbeat should be detected as stale
    let stale = job_query::find_stale_jobs(&conn, 60).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].status, JobStatus::Running);
}

#[test]
fn mark_stale_changes_status() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let run =
        job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    let job_concurrency = HashMap::new();
    job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();

    job_query::mark_stale(&conn, &run.id, "server restarted").unwrap();

    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Stale);
    assert_eq!(fetched.error.as_deref(), Some("server restarted"));
}
