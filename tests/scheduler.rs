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
use crap_cms::core::job::{JobDefinition, JobStatus};
use crap_cms::core::upload::{SharedStorage, storage::LocalStorage};
use crap_cms::db::query::jobs as job_query;
use crap_cms::db::{DbConnection, DbValue, migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::scheduler;

/// Build a no-op storage for tests that don't exercise upload conversion.
fn test_storage() -> SharedStorage {
    Arc::new(LocalStorage::new("/tmp/crap-cms-scheduler-test-storage"))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/job_tests")
}

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    Arc<crap_cms::core::Registry>,
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

// ── cron window claim: multi-server dedup ────────────────────────────────
//
// `try_claim_cron_window` is the only guard against a scheduled job firing
// once per server in a multi-server deployment. It must grant the first
// claim for a window and deny any subsequent claim for that same window
// (another node already fired it), then grant again on the next window.

#[test]
fn try_claim_cron_window_dedups_within_a_window() {
    let (_tmp, pool, _registry, _runner) = setup();
    let conn = pool.get().expect("DB connection");

    let window_one = "2026-01-01T00:00:00Z";

    // First claim for a never-fired slug → granted (INSERT path).
    let first = job_query::try_claim_cron_window(&conn, "sync", "2026-01-01T00:00:01Z", window_one)
        .expect("claim");
    assert!(first, "first claim for a new slug must be granted");

    // Second claim in the SAME window → denied: the stored fire time is not
    // before window_start, so the conditional UPDATE matches nothing.
    let second =
        job_query::try_claim_cron_window(&conn, "sync", "2026-01-01T00:00:02Z", window_one)
            .expect("claim");
    assert!(
        !second,
        "a second claim in the same window must be denied (multi-server dedup)"
    );

    // A claim for the NEXT window → granted again (UPDATE matches because the
    // stored fire time is before the new window_start).
    let next = job_query::try_claim_cron_window(
        &conn,
        "sync",
        "2026-01-01T01:00:01Z",
        "2026-01-01T01:00:00Z",
    )
    .expect("claim");
    assert!(next, "a claim for a later window must be granted");
}

// ── execute_job: successful execution ───────────────────────────────────

#[test]
fn execute_job_echo_completes_successfully() {
    let (_tmp, pool, registry, runner) = setup();

    // Insert and claim a job
    let conn = pool.get().expect("DB connection");
    let run = job_query::insert_job(
        &conn,
        "test_echo_job",
        "{\"hello\":\"world\"}",
        "manual",
        1,
        "default",
        0,
    )
    .expect("insert_job");
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = registry.get_job("test_echo_job").unwrap().clone();

    let job_run = &claimed[0];
    scheduler::execute_job(&pool, &runner, &job_def, job_run, None, &test_storage())
        .expect("execute_job");

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
        0,
    )
    .expect("insert_job");
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = registry.get_job("test_create_post").unwrap().clone();

    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0], None, &test_storage())
        .expect("execute_job");

    // Verify the document was created
    let def = registry.get_collection("posts").unwrap().clone();

    let conn = pool.get().expect("DB connection");
    let docs =
        query::find(&conn, "posts", &def, &query::FindQuery::default(), None).expect("find posts");
    assert!(!docs.is_empty(), "Job should have created a post");
    assert_eq!(
        docs[0].fields.get("title").and_then(|v| v.as_str()),
        Some("Scheduler Created")
    );

    // Verify the job run is completed
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Completed);
}

// ── execute_job: failing handler ────────────────────────────────────────

#[test]
fn execute_job_failing_handler_marks_failed() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    let run = job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 1, "default", 0)
        .expect("insert_job");
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = JobDefinition::builder("test_failing_job", "jobs.test_job.fail")
        .timeout(30)
        .build();

    // execute_job itself returns Ok — it handles the error internally
    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0], None, &test_storage())
        .expect("execute_job");

    // Verify the job is marked as failed (attempt 1, max_attempts 1 => no retry)
    let conn = pool.get().expect("DB connection");
    let fetched = job_query::get_job_run(&conn, &run.id).unwrap().unwrap();
    assert_eq!(fetched.status, JobStatus::Failed);
    assert!(
        fetched
            .error
            .as_ref()
            .unwrap()
            .contains("intentional failure")
    );
}

// ── execute_job: failing handler with retry ─────────────────────────────

#[test]
fn execute_job_failing_handler_retries() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    // max_attempts=3 so it should be retried
    let run = job_query::insert_job(&conn, "test_failing_job", "{}", "manual", 3, "default", 0)
        .expect("insert_job");
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    assert_eq!(claimed.len(), 1);
    drop(conn);

    let job_def = JobDefinition::builder("test_failing_job", "jobs.test_job.fail")
        .timeout(30)
        .build();

    // claimed[0].attempt = 1 (after claim), max_attempts = 3 => should_retry = true
    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0], None, &test_storage())
        .expect("execute_job");

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
    let run =
        job_query::insert_job(&conn, "test_echo_job", "{}", "manual", 1, "default", 0).unwrap();
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    assert_eq!(claimed.len(), 1);

    // Backdate the heartbeat to make it appear stale
    conn.execute(
        "UPDATE _crap_jobs SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-600 seconds') WHERE id = ?1",
        &[DbValue::Text(run.id.clone())],
    )
    .unwrap();

    // Recover stale jobs
    scheduler::recover_stale_jobs(&conn, &registry, 30).unwrap();

    let stale = job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0).unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].slug, "test_echo_job");
}

// ── check_cron_schedules integration ────────────────────────────────────

#[test]
fn check_cron_schedules_fires_test_cron_job() {
    let (_tmp, pool, registry, _runner) = setup();

    // Verify the test_cron_job definition was loaded
    {
        let def = registry
            .get_job("test_cron_job")
            .expect("test_cron_job should be defined");
        assert_eq!(def.schedule.as_deref(), Some("* * * * *"));
        assert!(def.skip_if_running);
    }

    let now = chrono::Utc::now();
    let last_check = now - chrono::Duration::minutes(2);

    scheduler::check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

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
        job_query::insert_job(&conn, "test_cron_job", "{}", "manual", 1, "default", 0).unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE slug = 'test_cron_job'",
            &[],
        )
        .unwrap();
    }

    let now = chrono::Utc::now();
    let last_check = now - chrono::Duration::minutes(2);

    scheduler::check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

    // test_cron_job has skip_if_running=true, so no new pending job
    let conn = pool.get().unwrap();
    let pending = job_query::list_job_runs(
        &conn,
        Some("test_cron_job"),
        Some(JobStatus::Pending),
        100,
        0,
    )
    .unwrap();
    assert_eq!(pending.len(), 0);

    // But test_cron_nonskip has skip_if_running=false, so it should have a pending job
    let nonskip = job_query::list_job_runs(
        &conn,
        Some("test_cron_nonskip"),
        Some(JobStatus::Pending),
        100,
        0,
    )
    .unwrap();
    assert_eq!(nonskip.len(), 1);
}

// ── crap.transaction(fn): commit + rollback semantics ──────────────────

/// Happy path for `crap.transaction(fn)`: two creates inside one tx
/// both end up visible after the job completes.
#[test]
fn tx_two_creates_both_committed() {
    let (_tmp, pool, registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    job_query::insert_job(
        &conn,
        "test_tx_two_creates",
        "{}",
        "manual",
        1,
        "default",
        0,
    )
    .expect("insert_job");
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    drop(conn);

    let job_def = registry.get_job("test_tx_two_creates").unwrap().clone();
    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0], None, &test_storage())
        .expect("execute_job");

    let posts_def = registry.get_collection("posts").unwrap().clone();
    let conn = pool.get().expect("DB connection");
    let docs = query::find(
        &conn,
        "posts",
        &posts_def,
        &query::FindQuery::default(),
        None,
    )
    .expect("find posts");

    let titles: Vec<&str> = docs
        .iter()
        .filter_map(|d| d.fields.get("title").and_then(|v| v.as_str()))
        .collect();
    assert!(
        titles.contains(&"tx-doc-1") && titles.contains(&"tx-doc-2"),
        "expected both tx-doc-1 and tx-doc-2 to be committed; got titles={titles:?}"
    );
}

/// Rollback path for `crap.transaction(fn)`: a create followed by
/// `error()` inside the tx leaves NO documents.
#[test]
fn tx_rollback_leaves_no_documents() {
    let (_tmp, pool, registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    job_query::insert_job(
        &conn,
        "test_tx_rollback_mid",
        "{}",
        "manual",
        1,
        "default",
        0,
    )
    .expect("insert_job");
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    drop(conn);

    let job_def = registry.get_job("test_tx_rollback_mid").unwrap().clone();
    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0], None, &test_storage())
        .expect("execute_job");

    let posts_def = registry.get_collection("posts").unwrap().clone();
    let conn = pool.get().expect("DB connection");
    let docs = query::find(
        &conn,
        "posts",
        &posts_def,
        &query::FindQuery::default(),
        None,
    )
    .expect("find posts");

    let leaked: Vec<&str> = docs
        .iter()
        .filter_map(|d| d.fields.get("title").and_then(|v| v.as_str()))
        .filter(|t| *t == "should-not-exist")
        .collect();
    assert!(
        leaked.is_empty(),
        "transaction rollback must remove all pending writes; found leaked: {leaked:?}"
    );
}

/// Pool-mode + per-block atomicity: two SEPARATE `crap.transaction`
/// blocks. The first commits; the second errors and rolls back. The
/// first must remain visible.
#[test]
fn tx_separate_blocks_first_commits_when_second_rolls_back() {
    let (_tmp, pool, registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    job_query::insert_job(
        &conn,
        "test_tx_separate_blocks",
        "{}",
        "manual",
        1,
        "default",
        0,
    )
    .expect("insert_job");
    let job_concurrency = HashMap::new();
    let claimed =
        job_query::claim_pending_jobs(&conn, 5, &job_concurrency, &HashMap::new(), 0).unwrap();
    drop(conn);

    let job_def = registry.get_job("test_tx_separate_blocks").unwrap().clone();
    scheduler::execute_job(&pool, &runner, &job_def, &claimed[0], None, &test_storage())
        .expect("execute_job");

    let posts_def = registry.get_collection("posts").unwrap().clone();
    let conn = pool.get().expect("DB connection");
    let docs = query::find(
        &conn,
        "posts",
        &posts_def,
        &query::FindQuery::default(),
        None,
    )
    .expect("find posts");

    let titles: Vec<&str> = docs
        .iter()
        .filter_map(|d| d.fields.get("title").and_then(|v| v.as_str()))
        .collect();
    assert!(
        titles.contains(&"block-1-doc"),
        "first crap.transaction block must commit; got titles={titles:?}"
    );
    assert!(
        !titles.contains(&"block-2-doc"),
        "second crap.transaction block must roll back; got titles={titles:?}"
    );
}
