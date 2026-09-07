//! Integration tests for transaction-outcome effects
//! (`crap.tx.on_commit` / `crap.tx.on_rollback`).
//!
//! Covers both commit points: the service pool-write envelope
//! (`run_pool_write`, driven via `service::create_document` with a
//! registering `before_change` hook) and `crap.transaction(fn)` in job
//! pool-mode (driven via `run_job_handler`).

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

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::{CollectionDefinition, Hooks};
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::job::JobRun;
use crap_cms::core::{DocumentFields, HookRef, Registry};
use crap_cms::db::{DbPool, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tx_outcome")
}

/// `tx_articles` with the given `before_change` hook ref.
fn tx_articles_def(hook_ref: &str) -> CollectionDefinition {
    let mut def = CollectionDefinition::new("tx_articles");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("boom", FieldType::Text).build(),
    ];
    def.hooks = Hooks {
        before_change: vec![HookRef::new(hook_ref)],
        ..Default::default()
    };
    def
}

/// `tx_log` — where effect handlers record what ran.
fn tx_log_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("tx_log");
    def.fields = vec![FieldDefinition::builder("message", FieldType::Text).build()];
    def
}

fn setup(hook_ref: &str) -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    setup_with(tx_articles_def(hook_ref))
}

/// `tx_articles` with an `after_read` hook instead of `before_change`.
fn tx_articles_after_read_def(hook_ref: &str) -> CollectionDefinition {
    let mut def = tx_articles_def(hook_ref);
    def.hooks = Hooks {
        after_read: vec![HookRef::new(hook_ref)],
        ..Default::default()
    };
    def
}

fn setup_with(
    articles: CollectionDefinition,
) -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(articles);
        reg.register_collection(tx_log_def());
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let fixture = fixture_dir();
    let runner = HookRunner::builder()
        .config_dir(&fixture)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    (tmp, db_pool, registry, runner)
}

fn create_article(
    pool: &DbPool,
    registry: &Arc<Registry>,
    runner: &HookRunner,
    data: DocumentFields,
) -> Result<String, service::ServiceError> {
    let def = registry.get_collection("tx_articles").unwrap().clone();
    let ctx = service::ServiceContext::collection("tx_articles", &def)
        .pool(pool)
        .runner(runner)
        .build();

    service::create_document(&ctx, service::WriteInput::builder(data).build())
        .map(|(doc, _)| doc.id.to_string())
}

fn log_messages(pool: &DbPool, registry: &Arc<Registry>) -> Vec<String> {
    let def = registry.get_collection("tx_log").unwrap().clone();
    let conn = pool.get().unwrap();
    let docs = query::find(&conn, "tx_log", &def, &query::FindQuery::default(), None).unwrap();

    docs.iter()
        .filter_map(|d| d.fields.get("message").and_then(|v| v.as_str()))
        .map(String::from)
        .collect()
}

fn article_titles(pool: &DbPool, registry: &Arc<Registry>) -> Vec<String> {
    let def = registry.get_collection("tx_articles").unwrap().clone();
    let conn = pool.get().unwrap();
    let docs = query::find(
        &conn,
        "tx_articles",
        &def,
        &query::FindQuery::default(),
        None,
    )
    .unwrap();

    docs.iter()
        .filter_map(|d| d.fields.get("title").and_then(|v| v.as_str()))
        .map(String::from)
        .collect()
}

// ── Service pool-write envelope ─────────────────────────────────────────

/// `on_commit` fires exactly once after a successful commit; the
/// `on_rollback` registration from the same transaction is dropped.
#[test]
fn on_commit_runs_after_service_commit() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register");

    let data: DocumentFields = [("title".into(), json!("A"))].into_iter().collect();
    create_article(&pool, &registry, &runner, data).expect("create should succeed");

    assert_eq!(log_messages(&pool, &registry), vec!["commit:A:commit"]);
    assert_eq!(article_titles(&pool, &registry), vec!["A"]);
}

/// A hook error rolls the write back; only `on_rollback` fires, and the
/// document does not exist.
#[test]
fn on_rollback_runs_when_hook_errors() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register");

    let data: DocumentFields = [("title".into(), json!("B")), ("boom".into(), json!("yes"))]
        .into_iter()
        .collect();
    let res = create_article(&pool, &registry, &runner, data);

    // Surfaces reclassify at their boundary (gRPC/MCP/admin); the hook's
    // message reaches the caller, the Lua traceback does not.
    let err = res
        .expect_err("hook error must fail the write")
        .reclassify("sqlite");
    assert!(
        matches!(&err, crap_cms::service::ServiceError::HookError(m) if m.contains("boom requested")),
        "the hook's message reaches the caller as a hook error: {err}"
    );
    let err = err.to_string();
    assert!(
        !err.contains("stack traceback"),
        "the Lua traceback stays in the server log, not the error: {err}"
    );
    assert_eq!(log_messages(&pool, &registry), vec!["rollback:B:rollback"]);
    assert!(
        article_titles(&pool, &registry).is_empty(),
        "rolled-back document must not exist"
    );
}

/// Registering an unresolvable ref fails the registering hook — and with it
/// the whole write (fail-closed at registration time).
#[test]
fn unresolvable_ref_fails_the_write() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register_bad_ref");

    let data: DocumentFields = [("title".into(), json!("C"))].into_iter().collect();
    let res = create_article(&pool, &registry, &runner, data);

    let err = res.expect_err("bad ref must fail the write").to_string();
    assert!(
        err.contains("crap.tx.on_commit"),
        "error should name the registration point: {err}"
    );
    assert!(log_messages(&pool, &registry).is_empty());
    assert!(article_titles(&pool, &registry).is_empty());
}

/// Effect execution is fail-open: a failing effect is logged and skipped,
/// later effects still run, and the committed write stands.
#[test]
fn failing_effect_is_skipped_others_run() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register_failing_effect");

    let data: DocumentFields = [("title".into(), json!("D"))].into_iter().collect();
    create_article(&pool, &registry, &runner, data).expect("create should succeed");

    assert_eq!(log_messages(&pool, &registry), vec!["commit:D:commit"]);
    assert_eq!(article_titles(&pool, &registry), vec!["D"]);
}

// ── Bare pool-mode CRUD (no crap.transaction) ───────────────────────────

/// A bare CRUD call in a job opens a per-op transaction with the FULL scope:
/// the collection's `before_change` hook can register `crap.tx.on_commit`,
/// and it runs after that op's commit — exactly as under `crap.transaction`
/// or the service envelope.
#[test]
fn bare_pool_mode_create_scopes_its_own_transaction() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register");

    let result = run_job(&runner, &pool, "jobs.tx_job.run_bare").expect("result json");
    let json: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["ok"], json!(true), "the bare create succeeds: {json}");

    assert_eq!(article_titles(&pool, &registry), vec!["bare"]);
    assert_eq!(log_messages(&pool, &registry), vec!["commit:bare:commit"]);
}

/// A hook error inside a bare op rolls that op back and runs only the
/// `on_rollback` compensation.
#[test]
fn bare_pool_mode_hook_error_rolls_the_op_back() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register");

    let result = run_job(&runner, &pool, "jobs.tx_job.run_bare_boom").expect("result json");
    let json: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(json["ok"], json!(false), "the create must fail: {json}");

    assert!(article_titles(&pool, &registry).is_empty());
    assert_eq!(
        log_messages(&pool, &registry),
        vec!["rollback:doomed:rollback"]
    );
}

/// `after_read` has no CRUD on any surface: a write attempted from it is
/// refused, and — `after_read` being fail-open — the read still succeeds.
#[test]
fn after_read_hook_cannot_write() {
    let (_tmp, pool, registry, runner) = setup_with(tx_articles_after_read_def(
        "hooks.effects.after_read_writes",
    ));
    let data: DocumentFields = [("title".into(), json!("E"))].into_iter().collect();
    create_article(&pool, &registry, &runner, data).expect("create");

    let result = run_job(&runner, &pool, "jobs.tx_job.run_find").expect("result json");
    let json: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        json["ok"],
        json!(true),
        "the read is not broken by the hook: {json}"
    );

    assert!(
        log_messages(&pool, &registry).is_empty(),
        "nothing may be written from after_read"
    );
}

// ── crap.transaction(fn) in job pool-mode ───────────────────────────────

fn run_job(runner: &HookRunner, pool: &DbPool, handler: &str) -> Option<String> {
    let run = JobRun::builder("tx-test-run", "tx_test")
        .data("{}")
        .attempt(1)
        .max_attempts(1)
        .build();

    runner
        .run_job_handler(&HookRef::new(handler), &run, pool, None)
        .expect("run_job_handler")
}

/// `crap.transaction` commit path: the in-tx create is durable and the
/// `on_commit` effects ran; `on_rollback` registrations were dropped.
///
/// Two commit entries prove queue propagation: the nested
/// `tx_articles.create` runs its `before_change` hook, whose own
/// `crap.tx` registrations attach to the SAME enclosing transaction.
#[test]
fn transaction_commit_runs_on_commit_effects() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register");

    run_job(&runner, &pool, "jobs.tx_job.run_commit");

    assert_eq!(article_titles(&pool, &registry), vec!["in-tx"]);

    let mut msgs = log_messages(&pool, &registry);
    msgs.sort();
    assert_eq!(msgs, vec!["commit:in-tx:commit", "commit:job:commit"]);
}

/// `crap.transaction` rollback path: the in-tx create is rolled back and
/// only the `on_rollback` compensation ran.
#[test]
fn transaction_rollback_runs_on_rollback_effects() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register");

    let result = run_job(&runner, &pool, "jobs.tx_job.run_rollback").expect("result json");
    let json: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(json.get("ok"), Some(&json!(false)), "tx must have failed");
    assert!(article_titles(&pool, &registry).is_empty());

    // Both the job's registration and the nested create's hook registration
    // fire their rollback compensations; no commit effect runs.
    let mut msgs = log_messages(&pool, &registry);
    msgs.sort();
    assert_eq!(
        msgs,
        vec!["rollback:doomed:rollback", "rollback:job:rollback"]
    );
}

/// Registration outside any transaction errors with guidance.
#[test]
fn registration_outside_transaction_errors() {
    let (_tmp, pool, registry, runner) = setup("hooks.effects.register");

    let result = run_job(&runner, &pool, "jobs.tx_job.run_no_tx").expect("result json");
    let json: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert_eq!(json.get("ok"), Some(&json!(false)));
    let err = json.get("err").and_then(|v| v.as_str()).unwrap_or_default();
    assert!(
        err.contains("requires an active write transaction"),
        "unexpected error: {err}"
    );
    assert!(log_messages(&pool, &registry).is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// Frozen contract: a rolled-back write never emits an event — including
// writes made INSIDE `crap.transaction(fn)` from a job handler. The
// transaction previously routed inner-CRUD events straight into the
// job-level queue, which flushes unconditionally post-handler.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn rolled_back_transaction_publishes_no_events_committed_one_does() {
    use crap_cms::core::SharedEventTransport;
    use crap_cms::core::event::InProcessEventBus;
    use crap_cms::hooks::lifecycle::LuaCrudInfra;
    use std::sync::Arc as StdArc;

    let (_tmp, pool, registry, _fixture_runner) = setup("hooks.tx.noop");

    // A runner whose config_dir is the TEMP dir, so the test can write
    // its own job-handler module there.
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let runner = HookRunner::builder()
        .config_dir(_tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("tmp runner");

    let bus: SharedEventTransport = StdArc::new(InProcessEventBus::new(64));
    let mut rx = bus.subscribe();

    // The collection's `before_change` ref must resolve on this runner —
    // provide the no-op module the setup registered.
    std::fs::create_dir_all(_tmp.path().join("hooks")).unwrap();
    std::fs::write(
        _tmp.path().join("hooks/tx.lua"),
        "local M = {}\nfunction M.noop(ctx) return ctx end\nreturn M\n",
    )
    .unwrap();

    let run_handler = |name: &str, lua_body: &str| {
        // Distinct module per invocation: `require` caches in
        // `package.loaded`, so re-writing the same filename would re-run
        // the FIRST body on a pooled VM.
        let handler_file = _tmp.path().join("hooks");
        std::fs::create_dir_all(&handler_file).unwrap();
        std::fs::write(
            handler_file.join(format!("{name}.lua")),
            format!("local M = {{}}\nfunction M.run(ctx)\n{lua_body}\nend\nreturn M"),
        )
        .unwrap();

        let job_run = JobRun::builder(format!("run-{name}"), name).build();
        let infra = LuaCrudInfra {
            event_transport: Some(bus.clone()),
            cache: None,
            event_queue: None,
            verification_queue: None,
            file_cleanup: None,
            cache_dirty: None,
            deferred: None,
        };
        runner
            .run_job_handler(
                &crap_cms::core::HookRef::new(format!("hooks.{name}.run")),
                &job_run,
                &pool,
                Some(infra),
            )
            .expect("handler runs");
    };

    // 1. Rolled-back transaction: the update MUST NOT publish.
    run_handler(
        "txjob_rollback",
        r#"
        local d = crap.collections.create("tx_articles", { title = "ev-rollback" })
        local ok = pcall(function()
            crap.transaction(function()
                crap.collections.update("tx_articles", d.id, { title = "changed" })
                error("boom")
            end)
        end)
        assert(not ok)
    "#,
    );

    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push(format!("{:?}:{}", ev.operation, ev.collection));
    }
    assert!(
        !seen.iter().any(|e| e.starts_with("Update")),
        "a rolled-back transaction must not publish its update event; got {seen:?}"
    );

    // 2. Committed transaction: the update MUST publish.
    run_handler(
        "txjob_commit",
        r#"
        local d = crap.collections.create("tx_articles", { title = "ev-commit" })
        crap.transaction(function()
            crap.collections.update("tx_articles", d.id, { title = "changed2" })
        end)
    "#,
    );

    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seen.push(format!("{:?}:{}", ev.operation, ev.collection));
    }
    assert!(
        seen.iter().any(|e| e.starts_with("Update")),
        "a committed transaction must publish its update event; got {seen:?}"
    );
}
