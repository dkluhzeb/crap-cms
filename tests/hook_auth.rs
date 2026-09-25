//! Auth hooks through the `HookRunner`: custom strategies, auth callbacks,
//! and `mfa_deliver` — their CRUD access and transaction contracts.

#![allow(
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines
)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use crap_cms::{
    config::CrapConfig,
    core::{
        Document, DocumentFields, EventReceiver, HookRef, Registry, SharedEventTransport,
        event::InProcessEventBus, upload::create_storage,
    },
    db::{BoxedConnection, DbConnection, DbPool, migrate, pool, query},
    hooks::{
        self,
        lifecycle::{AuthStrategyInput, HookRunner, MfaDeliverInput},
    },
    service::{AppInfra, StandaloneInfra},
};
use serde_json::json;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup() -> (tempfile::TempDir, Arc<AppInfra>) {
    let (tmp, infra, _events) = setup_in(&fixture_dir(), |_| {});

    (tmp, infra)
}

/// [`setup`] with a pool whose single write connection a test can hold, and
/// a one-second wait for it — so "this path never takes a write connection"
/// is observable (a path that does fails fast instead of succeeding).
fn setup_single_writer() -> (tempfile::TempDir, Arc<AppInfra>) {
    let (tmp, infra, _events) = setup_in(&fixture_dir(), |config| {
        config.database.write_pool_max_size = 1;
        config.database.connection_timeout = 1;
    });

    (tmp, infra)
}

/// An [`AppInfra`] over the fixture tree at `config_dir` (temp-dir `SQLite`
/// database, local storage, memory cache) with an in-process event bus, and
/// a receiver subscribed to that bus before any write.
fn setup_in(
    config_dir: &Path,
    configure: impl FnOnce(&mut CrapConfig),
) -> (tempfile::TempDir, Arc<AppInfra>, EventReceiver) {
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(config_dir, &config).expect("Failed to init Lua");

    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    configure(&mut pool_config);
    let db_pool = pool::create_pool(tmp.path(), &pool_config).expect("Failed to create pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("Failed to sync schema");

    let runner = HookRunner::builder()
        .config_dir(config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("Failed to create HookRunner");
    let storage = create_storage(tmp.path(), &config.upload).expect("storage");

    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
    let events = transport.subscribe();

    let infra = AppInfra::standalone(StandaloneInfra {
        pool: db_pool,
        registry,
        hook_runner: runner,
        storage,
        token_provider: None,
        event_transport: Some(transport),
        invalidation_transport: None,
        config: &config,
        config_dir,
    })
    .expect("build test infra");

    (tmp, infra, events)
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
    let (_tmp, infra) = setup();
    let (pool, registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);

    // Create an article (auth_strategy.lua looks up articles to return a user-like doc)
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("Strategy Test"));
    let _doc = create_article(pool, registry, &data);

    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "valid-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner.run_auth_strategy(
        &HookRef::new("hooks.auth_strategy.api_key_auth"),
        &api_key_strategy_input(&headers),
        &conn,
        &infra,
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
    let (_tmp, infra) = setup();
    let (pool, _registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);

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
            &infra,
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
            &infra,
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
    let (_tmp, infra) = setup();
    let (pool, _registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);

    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "wrong-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.api_key_auth"),
            &api_key_strategy_input(&headers),
            &conn,
            &infra,
        )
        .expect("should not error");
    assert!(result.is_none(), "Invalid key should return None");
}

#[test]
fn auth_strategy_returns_none_on_missing_header() {
    let (_tmp, infra) = setup();
    let (pool, _registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);

    let headers: HashMap<String, String> = HashMap::new(); // no x-api-key header

    let conn = pool.get().expect("DB connection");
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.api_key_auth"),
            &api_key_strategy_input(&headers),
            &conn,
            &infra,
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
    let (_tmp, infra) = setup();
    let (pool, _registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);
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
            &infra,
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
            &infra,
        )
        .expect("should not error");
    assert!(
        denied.is_none(),
        "strategy must reject a wrong password (proves ctx.password reaches it)"
    );
}

#[test]
fn auth_strategy_has_crud_access() {
    let (_tmp, infra) = setup();
    let (pool, registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);

    // Create two articles for the strategy to find
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("First Article"));
    let _doc = create_article(pool, registry, &data);
    data.insert("title".to_string(), json!("Second Article"));
    let _doc = create_article(pool, registry, &data);

    // The strategy calls crap.collections.find — test that it works
    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "valid-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.api_key_auth"),
            &api_key_strategy_input(&headers),
            &conn,
            &infra,
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
    let (_tmp, infra) = setup();
    let (pool, _registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);
    let hook = HookRef::new("hooks.auth_strategy.writing_auth");
    let before = article_count(pool);

    let headers = HashMap::new();
    let result = runner
        .run_auth_callback(&hook, &api_key_strategy_input(&headers), &infra)
        .expect("should not error");
    assert!(result.is_none());
    assert_eq!(article_count(pool), before, "a failed callback rolls back");

    let mut headers = HashMap::new();
    headers.insert("x-succeed".to_string(), "yes".to_string());
    let result = runner
        .run_auth_callback(&hook, &api_key_strategy_input(&headers), &infra)
        .expect("should not error");
    assert!(result.is_some());
    assert_eq!(
        article_count(pool),
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
    let (_tmp, infra) = setup_single_writer();
    let (pool, _registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);
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
            &infra,
        )
        .expect("a callback without CRUD needs no write connection");
    assert!(user.is_some());

    let err = runner
        .run_auth_callback(
            &HookRef::new("hooks.auth_strategy.writing_auth"),
            &input,
            &infra,
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
    let (_tmp, infra) = setup_single_writer();
    let (pool, _registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);
    let user = Document::new("u1");
    let writing = HookRef::new("hooks.auth_strategy.deliver_writing");
    let before = article_count(pool);

    runner
        .run_mfa_deliver(&writing, &mfa_deliver_input(&user, "123456"), &infra)
        .expect("delivery succeeds");
    assert_eq!(article_count(pool), before + 1, "a delivered hook commits");

    runner
        .run_mfa_deliver(&writing, &mfa_deliver_input(&user, "000000"), &infra)
        .expect_err("the hook raises");
    assert_eq!(article_count(pool), before + 1, "a failed hook rolls back");

    let held = pool.write().expect("the only write connection");
    runner
        .run_mfa_deliver(
            &HookRef::new("hooks.auth_strategy.deliver_noop"),
            &mfa_deliver_input(&user, "123456"),
            &infra,
        )
        .expect("a hook without CRUD needs no write connection");
    drop(held);
}

// ── The auth hooks' transaction scope ─────────────────────────────────────

fn auth_tx_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/auth_hook_tx")
}

fn auth_tx_setup() -> (tempfile::TempDir, Arc<AppInfra>, EventReceiver) {
    setup_in(&auth_tx_fixture_dir(), |_| {})
}

fn named_input(headers: &HashMap<String, String>) -> AuthStrategyInput<'_> {
    AuthStrategyInput {
        collection: "members",
        headers,
        email: None,
        password: None,
        remote_addr: None,
    }
}

fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// The `name` / `message` column of every row of `table`, sorted.
fn column(pool: &DbPool, table: &str, col: &str) -> Vec<String> {
    let mut values: Vec<String> = pool
        .get()
        .unwrap()
        .query_all(&format!("SELECT {col} FROM {table}"), &[])
        .unwrap()
        .iter()
        .filter_map(|r| r.get_string(col).ok())
        .collect();
    values.sort();

    values
}

/// The collections of every event published so far.
fn published(events: &mut EventReceiver) -> Vec<String> {
    let mut collections = Vec::new();

    while let Ok(event) = events.try_recv() {
        collections.push(event.collection.to_string());
    }

    collections
}

/// Regression: an auth callback's CRUD ran without any transaction scope. A
/// user provisioned on first sign-in published no live event, and a
/// collection `after_change` hook calling `crap.tx.on_commit` failed with
/// "requires an active write transaction" — so provisioning itself failed.
/// The callback's transaction now has the scope every write surface has.
#[test]
fn a_callback_provisioning_a_user_publishes_and_runs_commit_effects() {
    let (_tmp, infra, mut events) = auth_tx_setup();
    let hook = HookRef::new("hooks.members.provision");
    let headers = headers(&[("x-name", "alice")]);

    let user = infra
        .hook_runner
        .run_auth_callback(&hook, &named_input(&headers), &infra)
        .expect("provisioning succeeds");

    assert!(user.is_some());
    assert_eq!(column(&infra.pool, "members", "name"), vec!["alice"]);
    assert_eq!(
        column(&infra.pool, "member_log", "message"),
        vec!["commit:alice"],
        "the after_change hook's on_commit effect ran after the commit"
    );
    assert!(
        published(&mut events).contains(&"members".to_string()),
        "the provisioned member's create event was published"
    );
}

/// A callback that authenticates no one rolls back: nothing is written,
/// published, or run as a commit effect.
#[test]
fn a_denied_callback_publishes_nothing() {
    let (_tmp, infra, mut events) = auth_tx_setup();
    let hook = HookRef::new("hooks.members.provision");
    let headers = headers(&[("x-name", "mallory"), ("x-deny", "yes")]);

    let user = infra
        .hook_runner
        .run_auth_callback(&hook, &named_input(&headers), &infra)
        .expect("the hook runs");

    assert!(user.is_none());
    assert!(column(&infra.pool, "members", "name").is_empty());
    assert!(column(&infra.pool, "member_log", "message").is_empty());
    assert!(published(&mut events).is_empty());
}

/// Regression: a CRUD call that failed after writing — caught by the hook
/// with `pcall` — left its partial writes in the shared transaction, which
/// then committed them (and, on Postgres, a failed statement made the
/// commit silently roll back everything). Each call is now one atomic step:
/// the failed call leaves no row, no event and no commit effect behind,
/// while the rest of the hook commits.
#[test]
fn a_caught_failed_crud_call_leaves_nothing_behind() {
    let (_tmp, infra, mut events) = auth_tx_setup();
    let hook = HookRef::new("hooks.members.provision_after_caught_failure");
    let headers = headers(&[("x-name", "bob")]);

    let user = infra
        .hook_runner
        .run_auth_callback(&hook, &named_input(&headers), &infra)
        .expect("the hook recovers from the failed call");

    assert!(user.is_some());
    assert_eq!(
        column(&infra.pool, "members", "name"),
        vec!["bob"],
        "the failed call's member row was rolled back"
    );
    assert_eq!(
        column(&infra.pool, "member_log", "message"),
        vec!["commit:bob"],
        "the failed call's on_commit effect was dropped"
    );
    let members_events = published(&mut events)
        .into_iter()
        .filter(|c| c == "members")
        .count();
    assert_eq!(
        members_events, 1,
        "only the committed member's event was published"
    );
}

/// An `mfa_deliver` hook's writes get the same scope.
#[test]
fn an_mfa_deliver_write_publishes_and_runs_commit_effects() {
    let (_tmp, infra, mut events) = auth_tx_setup();
    let user = Document::new("u1");
    let input = MfaDeliverInput {
        collection: "members",
        user: &user,
        code: "123456",
        expires_in: 300,
    };

    infra
        .hook_runner
        .run_mfa_deliver(&HookRef::new("hooks.members.deliver"), &input, &infra)
        .expect("delivery succeeds");

    assert_eq!(
        column(&infra.pool, "member_log", "message"),
        vec!["commit:delivered:123456"]
    );
    assert!(published(&mut events).contains(&"members".to_string()));
}

/// A strategy's transaction on the caller's connection gets the same scope,
/// and leaves that connection in autocommit.
#[test]
fn a_strategy_provisioning_a_user_publishes_and_runs_commit_effects() {
    let (_tmp, infra, mut events) = auth_tx_setup();
    let hook = HookRef::new("hooks.members.provision");
    let headers = headers(&[("x-name", "carol")]);
    let conn = infra.pool.get().expect("the request's connection");

    let user = infra
        .hook_runner
        .run_auth_strategy(&hook, &named_input(&headers), &conn, &infra)
        .expect("provisioning succeeds");

    assert!(user.is_some());
    assert!(!conn.in_transaction(), "the transaction was settled");
    assert_eq!(
        column(&infra.pool, "member_log", "message"),
        vec!["commit:carol"]
    );
    assert!(published(&mut events).contains(&"members".to_string()));
}

/// Regression: a strategy ran its transaction on the connection it was
/// handed — a login had to hand it a write connection, held across the
/// hook's network I/O. Its reads now run on the caller's connection, and
/// only a write takes a write connection: a strategy that only looks its
/// user up succeeds while the only write connection is held elsewhere, one
/// that writes waits for it.
#[test]
fn a_strategy_takes_a_write_connection_only_when_it_writes() {
    let (_tmp, infra) = setup_single_writer();
    let (pool, registry, runner) = (&infra.pool, &infra.registry, &infra.hook_runner);

    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("Reader"));
    create_article(pool, registry, &data);

    let conn = pool.get().expect("the request's read connection");
    let held = pool.write().expect("the only write connection");

    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "valid-key".to_string());
    let user = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.api_key_auth"),
            &api_key_strategy_input(&headers),
            &conn,
            &infra,
        )
        .expect("a strategy that only reads needs no write connection");
    assert!(user.is_some());

    let err = runner
        .run_auth_strategy(
            &HookRef::new("hooks.auth_strategy.writing_auth"),
            &api_key_strategy_input(&headers),
            &conn,
            &infra,
        )
        .expect_err("a strategy that writes waits for the write connection");
    assert!(
        format!("{err:#}").contains("no write connection"),
        "{err:#}"
    );

    drop(held);
}
