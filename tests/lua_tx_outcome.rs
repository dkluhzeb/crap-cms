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
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(tx_articles_def(hook_ref));
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

    assert!(res.is_err(), "hook error must fail the write");
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
