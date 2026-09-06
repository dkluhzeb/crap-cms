//! Integration tests for custom routes (`crap.routes.register`): the registry is
//! extracted from a real config dir, and handlers run through the live
//! `HookRunner` in pool-mode — exercising the Lua execution path end to end
//! (echo, pool-mode CRUD via `crap.collections.*`, and the access gate).

#![allow(clippy::missing_panics_doc)]

use std::collections::HashMap;
use std::sync::Arc;

use crap_cms::admin::custom_routes::{RouteBody, RouteResponse};
use crap_cms::config::CrapConfig;
use crap_cms::core::HookRef;
use crap_cms::db::{DbPool, migrate, pool};
use crap_cms::hooks::{self, lifecycle::HookRunner, lifecycle::RouteHandlerInput};
use serde_json::json;

/// Build a config dir with the given files, init Lua, and return a runner + pool.
fn setup(files: &[(&str, &str)]) -> (tempfile::TempDir, DbPool, HookRunner) {
    let tmp = tempfile::tempdir().expect("tempdir");
    for (rel, contents) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    (tmp, db_pool, runner)
}

fn input(method: &str, json_body: Option<serde_json::Value>) -> RouteHandlerInput {
    RouteHandlerInput {
        method: method.to_string(),
        path: "/test".to_string(),
        params: HashMap::new(),
        query: HashMap::from([("x".to_string(), "42".to_string())]),
        headers: HashMap::new(),
        cookies: HashMap::new(),
        body: None,
        json: json_body,
        form: None,
        user: None,
        collection: None,
        ip: "127.0.0.1".to_string(),
        ui_locale: None,
        options: None,
    }
}

#[test]
fn extracts_registered_route_metadata() {
    let (_tmp, _pool, runner) = setup(&[
        (
            "init.lua",
            r#"crap.routes.register({ path = "/echo", method = { "GET", "POST" }, handler = "routes.echo" })"#,
        ),
        (
            "routes/echo.lua",
            "return function(ctx) return { json = {} } end",
        ),
    ]);

    let routes = runner.extract_routes();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].path, "/echo");
    assert_eq!(
        routes[0].methods,
        vec!["GET".to_string(), "POST".to_string()]
    );
    assert_eq!(routes[0].handler.reference(), "routes.echo");
}

#[test]
fn handler_echoes_context_into_response() {
    let (_tmp, db_pool, runner) = setup(&[
        (
            "init.lua",
            r#"crap.routes.register({ path = "/echo", method = "POST", handler = "routes.echo" })"#,
        ),
        (
            "routes/echo.lua",
            r"return function(ctx)
                return {
                    status = 201,
                    json = { method = ctx.method, x = ctx.query.x, got = ctx.json.title },
                }
            end",
        ),
    ]);

    let resp = runner
        .run_route_handler(
            &HookRef::new("routes.echo"),
            &input("POST", Some(json!({ "title": "hello" }))),
            &db_pool,
            None,
        )
        .expect("handler ran");

    assert_eq!(resp.status, 201);
    let RouteBody::Json(v) = resp.body else {
        panic!("expected JSON body");
    };
    assert_eq!(v["method"], json!("POST"));
    assert_eq!(v["x"], json!("42"));
    assert_eq!(v["got"], json!("hello"));
}

#[test]
fn handler_can_do_pool_mode_crud() {
    let (_tmp, db_pool, runner) = setup(&[
        (
            "collections/items.lua",
            r#"crap.collections.define("items", { fields = { crap.fields.text({ name = "title" }) } })"#,
        ),
        (
            "init.lua",
            r#"crap.routes.register({ path = "/make", method = "POST", handler = "routes.make" })"#,
        ),
        (
            "routes/make.lua",
            r#"return function(ctx)
                local doc = crap.collections.create("items", { title = ctx.json.title })
                return { json = { id = doc.id, title = doc.title } }
            end"#,
        ),
    ]);

    let resp = runner
        .run_route_handler(
            &HookRef::new("routes.make"),
            &input("POST", Some(json!({ "title": "made-in-route" }))),
            &db_pool,
            None,
        )
        .expect("handler ran");

    let RouteBody::Json(v) = resp.body else {
        panic!("expected JSON body");
    };
    assert_eq!(v["title"], json!("made-in-route"));
    assert!(v["id"].is_string(), "create should return a document id");
}

#[test]
fn access_gate_allows_and_denies() {
    let (_tmp, db_pool, runner) = setup(&[
        (
            "init.lua",
            r#"crap.routes.register({ path = "/g", method = "GET", handler = "routes.g", access = "access.has_x" })"#,
        ),
        (
            "routes/g.lua",
            "return function(ctx) return { json = {} } end",
        ),
        // Allow only when query `x` is present.
        (
            "access/has_x.lua",
            "return function(ctx) return ctx.query.x ~= nil end",
        ),
    ]);

    let access = HookRef::new("access.has_x");

    // query has x=42 → allowed
    let allowed = runner
        .run_route_access(&access, &input("GET", None), &db_pool)
        .expect("access ran");
    assert!(allowed);

    // strip x → denied
    let mut no_x = input("GET", None);
    no_x.query.clear();
    let denied = runner
        .run_route_access(&access, &no_x, &db_pool)
        .expect("access ran");
    assert!(!denied);
}

#[test]
fn nil_return_is_404() {
    let (_tmp, db_pool, runner) = setup(&[
        (
            "init.lua",
            r#"crap.routes.register({ path = "/n", method = "GET", handler = "routes.n" })"#,
        ),
        ("routes/n.lua", "return function(ctx) return nil end"),
    ]);

    let resp: RouteResponse = runner
        .run_route_handler(
            &HookRef::new("routes.n"),
            &input("GET", None),
            &db_pool,
            None,
        )
        .expect("handler ran");
    assert_eq!(resp.status, 404);
}

#[test]
fn reserved_prefix_collision_fails_at_init() {
    // A free per-route path becomes reserved once the configured prefix is
    // applied (`/api` + `/posts` → `/api/posts`); must abort boot, not panic
    // later at router assembly.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("routes")).unwrap();
    std::fs::write(
        tmp.path().join("routes/x.lua"),
        "return function(ctx) return { json = {} } end",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("init.lua"),
        r#"crap.routes.register({ path = "/posts", method = "GET", handler = "routes.x" })"#,
    )
    .unwrap();

    let mut config = CrapConfig::test_default();
    config.routes.prefix = "/api".to_string();

    let result = hooks::init_lua(tmp.path(), &config);
    assert!(result.is_err(), "reserved-prefix collision must abort boot");
    let err = format!("{:#}", result.err().unwrap()); // full cause chain
    assert!(
        err.contains("reserved") || err.contains("collides"),
        "{err}"
    );
}

#[test]
fn bad_handler_ref_fails_at_init() {
    // validate_routes runs in init_lua; a dangling handler ref must abort boot.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("init.lua"),
        r#"crap.routes.register({ path = "/x", method = "GET", handler = "routes.missing" })"#,
    )
    .unwrap();
    let config = CrapConfig::test_default();
    let result = hooks::init_lua(tmp.path(), &config);
    assert!(
        result.is_err(),
        "dangling route handler ref must abort boot"
    );
    let err = result.err().unwrap().to_string();
    assert!(err.contains("route") || err.contains("missing"), "{err}");
}

/// Init-fails helper: write a routes/h.lua handler + the given init.lua and
/// prefix, then return the full error chain (panics if boot unexpectedly OK).
fn init_failure(init_lua: &str, prefix: &str) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("routes")).unwrap();
    std::fs::write(
        tmp.path().join("routes/h.lua"),
        "return function(ctx) return { json = {} } end",
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), init_lua).unwrap();

    let mut config = CrapConfig::test_default();
    config.routes.prefix = prefix.to_string();

    let result = hooks::init_lua(tmp.path(), &config);
    assert!(result.is_err(), "expected boot to fail");
    format!("{:#}", result.err().unwrap())
}

#[test]
fn duplicate_route_fails_at_init() {
    let err = init_failure(
        r#"
        crap.routes.register({ path = "/dup", method = "GET", handler = "routes.h" })
        crap.routes.register({ path = "/dup", method = "GET", handler = "routes.h" })
        "#,
        "",
    );
    assert!(err.contains("duplicate"), "{err}");
}

#[test]
fn get_and_explicit_head_same_path_fails_at_init() {
    // A GET route implicitly answers HEAD; a separate HEAD registration on the
    // same path would panic at router assembly, so it must be caught at boot.
    let err = init_failure(
        r#"
        crap.routes.register({ path = "/x", method = "GET", handler = "routes.h" })
        crap.routes.register({ path = "/x", method = "HEAD", handler = "routes.h" })
        "#,
        "",
    );
    assert!(err.contains("duplicate"), "{err}");
}

#[test]
fn malformed_prefix_fails_at_init() {
    // A prefix without a leading '/' would panic Router::route at assembly.
    let err = init_failure(
        r#"crap.routes.register({ path = "/x", method = "GET", handler = "routes.h" })"#,
        "api",
    );
    assert!(
        err.contains("absolute path") || err.contains("prefix"),
        "{err}"
    );
}
