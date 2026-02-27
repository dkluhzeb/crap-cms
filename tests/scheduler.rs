use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::job::{JobDefinition, JobStatus};
use crap_cms::db::{migrate, pool, query};
use crap_cms::db::query::jobs as job_query;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::scheduler;

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

    let runner = HookRunner::new(&config_dir, registry.clone(), &config)
        .expect("Failed to create HookRunner");
    (tmp, db_pool, registry, runner)
}

// ── execute_job: successful execution ───────────────────────────────────

#[test]
fn execute_job_echo_completes_successfully() {
    let (_tmp, pool, registry, runner) = setup();

    // Insert and claim a job
    let conn = pool.get().expect("DB connection");
    let run = job_query::insert_job(&conn, "test_echo_job", "{\"hello\":\"world\"}", "manual", 1, "default")
        .expect("insert_job");
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    let claimed = job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = {
        let reg = registry.read().unwrap();
        reg.get_job("test_echo_job").unwrap().clone()
    };

    let job_run = &claimed[0];
    scheduler::execute_job(&pool, &runner, &job_def, job_run).expect("execute_job");

    // Verify the job is marked as completed
    let conn = pool.get().expect("DB connection");
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Completed);
    assert!(fetched.result.is_some());
    let result: serde_json::Value = serde_json::from_str(fetched.result.as_ref().unwrap()).unwrap();
    assert_eq!(result.get("hello").unwrap().as_str().unwrap(), "world");
}

// ── execute_job: creates documents via CRUD ─────────────────────────────

#[test]
fn execute_job_creates_document() {
    let (_tmp, pool, registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    let run = job_query::insert_job(
        &conn,
        "test_create_post",
        "{\"title\":\"Scheduler Created\"}",
        "manual",
        1,
        "default",
    ).expect("insert_job");
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    let claimed = job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = {
        let reg = registry.read().unwrap();
        reg.get_job("test_create_post").unwrap().clone()
    };

    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0]).expect("execute_job");

    // Verify the document was created
    let reg = registry.read().unwrap();
    let def = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    let conn = pool.get().expect("DB connection");
    let docs = query::find(&conn, "posts", &def, &query::FindQuery::default(), None)
        .expect("find posts");
    assert!(!docs.is_empty(), "Job should have created a post");
    assert_eq!(docs[0].fields.get("title").and_then(|v| v.as_str()), Some("Scheduler Created"));

    // Verify the job run is completed
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Completed);
}

// ── execute_job: failing handler ────────────────────────────────────────

#[test]
fn execute_job_failing_handler_marks_failed() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    let run = job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 1, "default")
        .expect("insert_job");
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    let claimed = job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = JobDefinition {
        slug: "test_failing_job".to_string(),
        handler: "jobs.test_job.fail".to_string(),
        timeout: 30,
        ..Default::default()
    };

    // execute_job itself returns Ok — it handles the error internally
    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0]).expect("execute_job");

    // Verify the job is marked as failed (attempt 1, max_attempts 1 => no retry)
    let conn = pool.get().expect("DB connection");
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Failed);
    assert!(fetched.error.as_ref().unwrap().contains("intentional failure"));
}

// ── execute_job: failing handler with retry ─────────────────────────────

#[test]
fn execute_job_failing_handler_retries() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    // max_attempts=3 so it should be retried
    let run = job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 3, "default")
        .expect("insert_job");
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    let claimed = job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = JobDefinition {
        slug: "test_failing_job".to_string(),
        handler: "jobs.test_job.fail".to_string(),
        timeout: 30,
        ..Default::default()
    };

    // claimed[0].attempt = 1 (after claim), max_attempts = 3 => should_retry = true
    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0]).expect("execute_job");

    let conn = pool.get().expect("DB connection");
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    // Should be reset to pending for retry
    assert_eq!(fetched.status, JobStatus::Pending);
}

// ── recover_stale_jobs integration ──────────────────────────────────────

#[test]
fn recover_stale_jobs_on_full_setup() {
    let (_tmp, pool, registry, _runner) = setup();

    let conn = pool.get().expect("DB connection");

    // Insert and claim a job, then simulate server crash (leave it running with old heartbeat)
    let run = job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default").unwrap();
    let running_counts = job_query::count_running_per_slug(&conn).unwrap();
    let job_concurrency = std::collections::HashMap::new();
    let claimed = job_query::claim_pending_jobs(&conn, 5, &running_counts, &job_concurrency).unwrap();
    assert_eq!(claimed.len(), 1);

    // Backdate the heartbeat to make it appear stale
    conn.execute(
        "UPDATE _crap_jobs SET heartbeat_at = datetime('now', '-600 seconds') WHERE id = ?1",
        [&run.id],
    ).unwrap();

    // Recover stale jobs
    scheduler::recover_stale_jobs(&conn, &registry).unwrap();

    let stale = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].slug, "test_echo_job");
}

// ── check_cron_schedules integration ────────────────────────────────────

#[test]
fn check_cron_schedules_fires_test_cron_job() {
    let (_tmp, pool, registry, _runner) = setup();

    // Verify the test_cron_job definition was loaded
    {
        let reg = registry.read().unwrap();
        let def = reg.get_job("test_cron_job").expect("test_cron_job should be defined");
        assert_eq!(def.schedule.as_deref(), Some("* * * * *"));
        assert!(def.skip_if_running);
    }

    let now = chrono::Utc::now();
    let last_check = now - chrono::Duration::minutes(2);

    scheduler::check_cron_schedules(&pool, &registry, last_check, now).unwrap();

    let conn = pool.get().unwrap();
    let jobs = job_query::list_job_runs(&conn, Some("test_cron_job"), None, 100, 0).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, JobStatus::Pending);
    assert_eq!(jobs[0].scheduled_by.as_deref(), Some("cron"));
}

#[test]
fn check_cron_schedules_skip_if_running_integration() {
    let (_tmp, pool, registry, _runner) = setup();

    // Insert a running job for the cron job
    {
        let conn = pool.get().unwrap();
        job_query::insert_job(&conn, "test_cron_job", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE slug = 'test_cron_job'",
            [],
        ).unwrap();
    }

    let now = chrono::Utc::now();
    let last_check = now - chrono::Duration::minutes(2);

    scheduler::check_cron_schedules(&pool, &registry, last_check, now).unwrap();

    // test_cron_job has skip_if_running=true, so no new pending job
    let conn = pool.get().unwrap();
    let pending = job_query::list_job_runs(&conn, Some("test_cron_job"), Some("pending"), 100, 0).unwrap();
    assert_eq!(pending.len(), 0);

    // But test_cron_nonskip has skip_if_running=false, so it should have a pending job
    let nonskip = job_query::list_job_runs(&conn, Some("test_cron_nonskip"), Some("pending"), 100, 0).unwrap();
    assert_eq!(nonskip.len(), 1);
}
