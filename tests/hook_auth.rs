//! Auth hooks through the `HookRunner`: custom strategies, auth callbacks,
//! and `mfa_deliver` — their CRUD access and transaction contracts.

#![allow(
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines
)]

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crap_cms::{
    config::CrapConfig,
    core::{Document, DocumentFields, HookRef, Registry},
    db::{BoxedConnection, DbConnection, DbPool, migrate, pool, query},
    hooks::{
        self,
        lifecycle::{AuthStrategyInput, HookRunner, MfaDeliverInput},
    },
};
use serde_json::json;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup() -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    setup_with_pool(|_| {})
}

/// [`setup`] with a pool whose single write connection a test can hold, and
/// a one-second wait for it — so "this path never takes a write connection"
/// is observable (a path that does fails fast instead of succeeding).
fn setup_single_writer() -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    setup_with_pool(|config| {
        config.database.write_pool_max_size = 1;
        config.database.connection_timeout = 1;
    })
}

fn setup_with_pool(
    configure: impl FnOnce(&mut CrapConfig),
) -> (tempfile::TempDir, DbPool, Arc<Registry>, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).expect("Failed to init Lua");

    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    configure(&mut pool_config);
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

fn create_article(pool: &DbPool, registry: &Arc<Registry>, data: &DocumentFields) -> Document {
    let def = registry
        .get_collection("articles")
        .expect("articles not found")
        .clone();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "articles", &def, data, None).expect("Create failed");
    tx.commit().expect("Commit");
    doc
}

fn api_key_strategy_input(headers: &HashMap<String, String>) -> AuthStrategyInput<'_> {
    AuthStrategyInput {
        collection: "articles",
        headers,
        email: None,
        password: None,
        remote_addr: None,
    }
}

#[test]
fn auth_strategy_returns_user_on_valid_key() {
    let (_tmp, pool, registry, runner) = setup();

    // Create an article (auth_strategy.lua looks up articles to return a user-like doc)
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("Strategy Test"));
    let _doc = create_article(&pool, &registry, &data);

    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "valid-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner.run_auth_strategy(
        &HookRef::new("hooks.auth_strategy.api_key_auth"),
        &api_key_strategy_input(&headers),
        &conn,
    );
    assert!(result.is_ok(), "run_auth_strategy should not error");
    let doc = result.unwrap();
    assert!(doc.is_some(), "Valid key should return a document");
}

/// Regression: strategy side-effect writes used to persist on FAILED
/// attempts (bare conn, no transaction) — attacker-controlled DB growth
/// from the login endpoint. The strategy now runs in a transaction that
/// commits only when it authenticates someone.
#[test]
fn auth_strategy_writes_roll_back_on_failed_attempt() {
    let (_tmp, pool, _registry, runner) = setup();

    let count = |conn: &BoxedConnection| -> i64 {
        conn.query_one("SELECT COUNT(*) AS c FROM articles", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap()
    };

    let conn = pool.get().expect("DB connection");
    let before = count(&conn);

    // Failed attempt (no x-succeed header): the article write must vanish.
    let headers = HashMap::new();
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.writing_auth"),
            &api_key_strategy_input(&headers),
            &conn,
        )
        .expect("should not error");
    assert!(result.is_none());
    assert_eq!(
        count(&conn),
        before,
        "failed strategy attempt must roll back its writes"
    );

    // Successful attempt: the write commits.
    let mut headers = HashMap::new();
    headers.insert("x-succeed".to_string(), "yes".to_string());
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.writing_auth"),
            &api_key_strategy_input(&headers),
            &conn,
        )
        .expect("should not error");
    assert!(result.is_some());
    assert_eq!(
        count(&conn),
        before + 1,
        "successful strategy attempt must commit its writes"
    );
}

#[test]
fn auth_strategy_returns_none_on_invalid_key() {
    let (_tmp, pool, _registry, runner) = setup();

    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "wrong-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.api_key_auth"),
            &api_key_strategy_input(&headers),
            &conn,
        )
        .expect("should not error");
    assert!(result.is_none(), "Invalid key should return None");
}

#[test]
fn auth_strategy_returns_none_on_missing_header() {
    let (_tmp, pool, _registry, runner) = setup();

    let headers: HashMap<String, String> = HashMap::new(); // no x-api-key header

    let conn = pool.get().expect("DB connection");
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.api_key_auth"),
            &api_key_strategy_input(&headers),
            &conn,
        )
        .expect("should not error");
    assert!(result.is_none(), "Missing header should return None");
}

/// Regression: a custom auth strategy must receive the submitted credentials
/// (`ctx.email` / `ctx.password`). The gRPC login passed an empty context and
/// no credentials, so a "verify against LDAP / external API" strategy was
/// impossible there.
#[test]
fn auth_strategy_receives_credentials() {
    let (_tmp, pool, _registry, runner) = setup();
    let headers = HashMap::new();
    let conn = pool.get().expect("DB connection");

    let good = AuthStrategyInput {
        collection: "articles",
        headers: &headers,
        email: Some("admin@x.com"),
        password: Some("secret"),
        remote_addr: Some("1.2.3.4"),
    };
    let ok = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.credential_auth"),
            &good,
            &conn,
        )
        .expect("should not error");
    assert!(
        ok.is_some(),
        "strategy must authenticate when ctx.email + ctx.password match"
    );

    let bad = AuthStrategyInput {
        collection: "articles",
        headers: &headers,
        email: Some("admin@x.com"),
        password: Some("wrong"),
        remote_addr: None,
    };
    let denied = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.credential_auth"),
            &bad,
            &conn,
        )
        .expect("should not error");
    assert!(
        denied.is_none(),
        "strategy must reject a wrong password (proves ctx.password reaches it)"
    );
}

#[test]
fn auth_strategy_has_crud_access() {
    let (_tmp, pool, registry, runner) = setup();

    // Create two articles for the strategy to find
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("First Article"));
    let _doc = create_article(&pool, &registry, &data);
    data.insert("title".to_string(), json!("Second Article"));
    let _doc = create_article(&pool, &registry, &data);

    // The strategy calls crap.collections.find — test that it works
    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "valid-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.api_key_auth"),
            &api_key_strategy_input(&headers),
            &conn,
        )
        .expect("should not error");
    assert!(
        result.is_some(),
        "Strategy with CRUD access should find articles and return one"
    );
}

fn article_count(pool: &DbPool) -> i64 {
    pool.get()
        .unwrap()
        .query_one("SELECT COUNT(*) AS c FROM articles", &[])
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap()
}

/// An auth callback keeps the strategy contract — its writes commit only
/// when it authenticates someone — on a transaction it opens at its first
/// CRUD call.
#[test]
fn auth_callback_writes_commit_only_on_success() {
    let (_tmp, pool, _registry, runner) = setup();
    let hook = HookRef::new("hooks.auth_strategy.writing_auth");
    let before = article_count(&pool);

    let headers = HashMap::new();
    let result = runner
        .run_auth_callback(&hook, &api_key_strategy_input(&headers), &pool)
        .expect("should not error");
    assert!(result.is_none());
    assert_eq!(article_count(&pool), before, "a failed callback rolls back");

    let mut headers = HashMap::new();
    headers.insert("x-succeed".to_string(), "yes".to_string());
    let result = runner
        .run_auth_callback(&hook, &api_key_strategy_input(&headers), &pool)
        .expect("should not error");
    assert!(result.is_some());
    assert_eq!(
        article_count(&pool),
        before + 1,
        "a successful callback commits"
    );
}

/// Regression: an auth callback held a write connection for its whole run —
/// across its outbound HTTP — whether it touched the database or not. A
/// callback without CRUD now never takes one (it succeeds while the only
/// write connection is held elsewhere), and one with CRUD takes it only at
/// that call.
#[test]
fn auth_callback_takes_a_write_connection_only_at_its_first_crud_call() {
    let (_tmp, pool, _registry, runner) = setup_single_writer();
    let headers = HashMap::new();
    let input = AuthStrategyInput {
        collection: "articles",
        headers: &headers,
        email: Some("admin@x.com"),
        password: Some("secret"),
        remote_addr: None,
    };

    let held = pool.write().expect("the only write connection");

    let user = runner
        .run_auth_callback(
            &HookRef::new("hooks.auth_strategy.credential_auth"),
            &input,
            &pool,
        )
        .expect("a callback without CRUD needs no write connection");
    assert!(user.is_some());

    let err = runner
        .run_auth_callback(
            &HookRef::new("hooks.auth_strategy.writing_auth"),
            &input,
            &pool,
        )
        .expect_err("a callback that writes waits for the write connection");
    assert!(
        format!("{err:#}").contains("no write connection"),
        "{err:#}"
    );

    drop(held);
}

fn mfa_deliver_input<'a>(user: &'a Document, code: &'a str) -> MfaDeliverInput<'a> {
    MfaDeliverInput {
        collection: "articles",
        user,
        code,
        expires_in: 300,
    }
}

/// An `mfa_deliver` hook's writes commit when it returns and roll back when
/// it errors; a hook without CRUD never takes a write connection (the code
/// is stored before it runs, so its delivery I/O holds none).
#[test]
fn mfa_deliver_opens_its_transaction_lazily() {
    let (_tmp, pool, _registry, runner) = setup_single_writer();
    let user = Document::new("u1");
    let writing = HookRef::new("hooks.auth_strategy.deliver_writing");
    let before = article_count(&pool);

    runner
        .run_mfa_deliver(&writing, &mfa_deliver_input(&user, "123456"), &pool)
        .expect("delivery succeeds");
    assert_eq!(article_count(&pool), before + 1, "a delivered hook commits");

    runner
        .run_mfa_deliver(&writing, &mfa_deliver_input(&user, "000000"), &pool)
        .expect_err("the hook raises");
    assert_eq!(article_count(&pool), before + 1, "a failed hook rolls back");

    let held = pool.write().expect("the only write connection");
    runner
        .run_mfa_deliver(
            &HookRef::new("hooks.auth_strategy.deliver_noop"),
            &mfa_deliver_input(&user, "123456"),
            &pool,
        )
        .expect("a hook without CRUD needs no write connection");
    drop(held);
}
