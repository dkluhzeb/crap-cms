//! Cross-surface **behavioral** parity (Phase 1: gRPC ↔ Lua;
//! Phase 2: + MCP, driven in-process through the public JSON-RPC entry).
//!
//! The routing guard proves surfaces delegate to the service layer, and the
//! capability guard proves every op exists on every surface. This suite proves
//! the surfaces actually *behave the same*: for the same data and query, the
//! gRPC and Lua adapters must return identical results. That's the layer that
//! would have caught "count disagrees with find" / "count ignores a filter on
//! one surface" — adapter-level drift the source-scan guards can't see.
//!
//! All surfaces are driven in-process over a **shared registry + pool**, so
//! a write through one is visible to the others. MCP is driven through
//! `McpServer::handle_message` — the same JSON-RPC dispatch the stdio and
//! HTTP transports use — so tool-name routing, argument parsing, and the
//! result envelope are all in the loop. (Admin HTTP stays with the browser
//! e2e suite: its behavior is form/render-shaped, and its write paths are
//! pinned to the same op bodies by `surface_parity.rs`.)

use std::path::PathBuf;
use std::sync::Arc;

use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
use crap_cms::config::CrapConfig;
use crap_cms::core::email::EmailRenderer;
use crap_cms::db::{DbPool, migrate, pool};
use crap_cms::hooks::{self, lifecycle::HookRunner};
use crap_cms::mcp::{JsonRpcRequest, McpServer};
use crap_cms::service::{AppInfra, StandaloneInfra};
use serde_json::{Value, json};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

/// Build a prost `Struct` from string key/value pairs (the gRPC `data` field).
fn make_struct(pairs: &[(&str, &str)]) -> content::DataMap {
    let fields = pairs
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_string(),
                content::FieldValue {
                    kind: Some(content::field_value::Kind::StringValue((*v).to_string())),
                },
            )
        })
        .collect();
    content::DataMap { fields }
}

/// A gRPC `ContentService` and a Lua `HookRunner` over one shared registry+pool.
struct ParityHarness {
    _tmp: tempfile::TempDir,
    service: ContentService,
    lua: HookRunner,
    mcp: McpServer,
    pool: DbPool,
}

fn harness() -> ParityHarness {
    let fixture = fixture_dir();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    // One registry (from the fixture's Lua collection defs), shared by both surfaces.
    let registry = hooks::init_lua(&fixture, &config).expect("init lua from fixture");

    let tmp = tempfile::tempdir().expect("tempdir");
    let pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync schema");

    // Two HookRunners over the same registry: one for the gRPC service (which
    // takes ownership), one the Lua surface drives directly.
    let build_runner = || {
        HookRunner::builder()
            .config_dir(&fixture)
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .expect("hook runner")
    };
    let lua = build_runner();
    let grpc_runner = build_runner();
    let mcp_runner = build_runner();

    let mcp_infra = AppInfra::standalone(StandaloneInfra {
        pool: pool.clone(),
        registry: Arc::clone(&registry),
        hook_runner: mcp_runner,
        storage: Arc::new(crap_cms::core::upload::storage::LocalStorage::new(
            tmp.path().join("uploads"),
        )),
        token_provider: None,
        event_transport: None,
        invalidation_transport: None,
        config: &config,
        config_dir: tmp.path(),
    })
    .expect("mcp infra");
    let mcp = McpServer {
        infra: mcp_infra,
        config: config.clone(),
        config_dir: tmp.path().to_path_buf(),
        client_name: std::sync::OnceLock::new(),
        transport_label: "(test)",
    };

    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let service = ContentService::new(
        ContentServiceDeps::builder()
            .pool(pool.clone())
            .registry(Arc::clone(&registry))
            .hook_runner(grpc_runner)
            .config(config.clone())
            .config_dir(tmp.path().to_path_buf())
            .storage(
                crap_cms::core::upload::create_storage(
                    tmp.path(),
                    &crap_cms::config::UploadConfig::default(),
                )
                .unwrap(),
            )
            .email_renderer(email_renderer)
            .login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
                5, 300,
            )))
            .ip_login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
                20, 300,
            )))
            .forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
                3, 900,
            )))
            .ip_forgot_password_limiter(Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(20, 900),
            ))
            .cache(Arc::new(crap_cms::core::cache::NoneCache))
            .token_provider(Arc::new(crap_cms::core::auth::JwtTokenProvider::new(
                "test-jwt-secret",
            )))
            .password_provider(Arc::new(crap_cms::core::auth::Argon2PasswordProvider))
            .build(),
    );

    ParityHarness {
        _tmp: tmp,
        service,
        lua,
        mcp,
        pool,
    }
}

impl ParityHarness {
    fn lua_eval(&self, code: &str) -> String {
        let conn = self.pool.get().expect("conn");
        self.lua
            .eval_lua_with_conn(code, &conn, None)
            .expect("lua eval")
    }

    /// Whether a gRPC create succeeds (`true`) or is rejected (`false`).
    async fn grpc_create_ok(&self, pairs: &[(&str, &str)]) -> bool {
        self.service
            .create(Request::new(content::CreateRequest {
                events: None,
                collection: "articles".to_string(),
                data: Some(make_struct(pairs)),
                ..Default::default()
            }))
            .await
            .is_ok()
    }

    /// Whether a Lua create succeeds (`true`) or is rejected (`false`).
    /// Wrapped in `pcall` so a validation error surfaces as `false` rather
    /// than aborting the eval.
    fn lua_create_ok(&self, lua_fields: &str) -> bool {
        self.lua_eval(&format!(
            "local ok = pcall(function() crap.collections.create(\"articles\", {lua_fields}) end); \
             return ok and \"ok\" or \"err\""
        )) == "ok"
    }

    /// Create an `articles` row via the Lua surface (shared DB).
    fn seed(&self, lua_fields: &str) {
        let id = self.lua_eval(&format!(
            "local d = crap.collections.create(\"articles\", {lua_fields}); return d.id"
        ));
        assert!(!id.is_empty(), "seed create returned no id");
    }

    fn lua_count(&self, where_frag: &str) -> i64 {
        self.lua_eval(&format!(
            "return tostring(crap.collections.count(\"articles\", {{ {where_frag} }}))"
        ))
        .parse()
        .expect("lua count int")
    }

    /// Drive one MCP tool through the public JSON-RPC dispatch. Returns
    /// the parsed JSON payload on success, `Err(text)` on a tool error
    /// (`isError: true`) or protocol error.
    fn mcp_call(&self, tool: &str, arguments: &Value) -> Result<Value, String> {
        let req: JsonRpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }))
        .expect("valid request");

        let resp = self.mcp.handle_message(req);
        if let Some(err) = resp.error {
            return Err(format!("protocol error: {}", err.message));
        }
        let result = resp.result.expect("result present");
        let text = result["content"][0]["text"]
            .as_str()
            .expect("text content")
            .to_string();
        if result["isError"] == json!(true) {
            return Err(text);
        }
        serde_json::from_str(&text).map_err(|e| format!("non-JSON payload: {e}: {text}"))
    }

    fn mcp_count(&self, args: &Value) -> i64 {
        self.mcp_call("count_articles", args).expect("mcp count")["count"]
            .as_i64()
            .expect("count int")
    }

    fn mcp_find_total(&self, args: &Value) -> i64 {
        self.mcp_call("find_articles", args).expect("mcp find")["pagination"]["total_docs"]
            .as_i64()
            .expect("total int")
    }

    fn lua_find_total(&self, where_frag: &str) -> i64 {
        self.lua_eval(&format!(
            "return tostring(crap.collections.find(\"articles\", {{ {where_frag} }}).pagination.total_docs)"
        ))
        .parse()
        .expect("lua find total int")
    }

    async fn grpc_count(&self, where_json: Option<&str>) -> i64 {
        self.service
            .count(Request::new(content::CountRequest {
                collection: "articles".to_string(),
                r#where: where_json.map(String::from),
                ..Default::default()
            }))
            .await
            .expect("grpc count")
            .into_inner()
            .count
    }

    async fn grpc_find_total(&self, where_json: Option<&str>) -> i64 {
        self.service
            .find(Request::new(content::FindRequest {
                collection: "articles".to_string(),
                r#where: where_json.map(String::from),
                ..Default::default()
            }))
            .await
            .expect("grpc find")
            .into_inner()
            .pagination
            .expect("pagination present")
            .total_docs
    }

    fn seed_two_published_one_draft(&self) {
        self.seed(r#"{ title = "Alpha", body = "x", status = "published" }"#);
        self.seed(r#"{ title = "Beta", body = "y", status = "published" }"#);
        self.seed(r#"{ title = "Gamma", body = "z", status = "draft" }"#);
    }
}

/// Invariant 1: `count` agrees with `find`'s total, and gRPC agrees with Lua,
/// for an unfiltered query. (The cited bug: count missing/disagreeing per surface.)
#[tokio::test]
async fn count_and_find_totals_agree_across_grpc_lua_and_mcp() {
    let h = harness();
    h.seed_two_published_one_draft();

    let gc = h.grpc_count(None).await;
    let gf = h.grpc_find_total(None).await;
    let lc = h.lua_count("");
    let lf = h.lua_find_total("");
    let mc = h.mcp_count(&json!({}));
    let mf = h.mcp_find_total(&json!({}));

    assert_eq!(
        (gc, gf, lc, lf, mc, mf),
        (3, 3, 3, 3, 3, 3),
        "count/find totals must agree across surfaces \
         (grpc_count={gc}, grpc_find={gf}, lua_count={lc}, lua_find={lf}, \
          mcp_count={mc}, mcp_find={mf})"
    );
}

/// Invariant 1b: a `where` filter is honored identically by `count` and `find`
/// on both surfaces — catches "count ignores the filter on one surface".
#[tokio::test]
async fn filter_is_honored_identically_by_count_and_find_on_all_surfaces() {
    let h = harness();
    h.seed_two_published_one_draft();

    let gc = h.grpc_count(Some(r#"{"status":"published"}"#)).await;
    let gf = h.grpc_find_total(Some(r#"{"status":"published"}"#)).await;
    let lc = h.lua_count(r#"where = { status = "published" }"#);
    let lf = h.lua_find_total(r#"where = { status = "published" }"#);
    let mc = h.mcp_count(&json!({ "where": { "status": "published" } }));
    let mf = h.mcp_find_total(&json!({ "where": { "status": "published" } }));

    assert_eq!(
        (gc, gf, lc, lf, mc, mf),
        (2, 2, 2, 2, 2, 2),
        "filtered count/find must agree across surfaces \
         (grpc_count={gc}, grpc_find={gf}, lua_count={lc}, lua_find={lf}, \
          mcp_count={mc}, mcp_find={mf})"
    );
}

/// Invariant 5: validation must reject (or accept) identically on both
/// surfaces — a constraint enforced by one adapter but not the other would let
/// invalid data in through that surface. Uses the `title` field's
/// `required` + `unique` constraints. (Distinct valid titles per surface
/// because `title` is unique.)
#[tokio::test]
async fn validation_rejection_is_consistent_across_grpc_lua_and_mcp() {
    let h = harness();

    // Valid create (required title present) succeeds on both surfaces.
    assert!(
        h.grpc_create_ok(&[("title", "ValidViaGrpc"), ("status", "published")])
            .await
    );
    assert!(h.lua_create_ok(r#"{ title = "ValidViaLua", status = "published" }"#));

    // Missing the required `title` is rejected on both surfaces.
    assert!(
        !h.grpc_create_ok(&[("status", "published")]).await,
        "gRPC accepted a doc missing required title"
    );
    assert!(
        !h.lua_create_ok(r#"{ status = "published" }"#),
        "Lua accepted a doc missing required title"
    );

    // MCP mirrors both outcomes.
    assert!(
        h.mcp_call(
            "create_articles",
            &json!({ "title": "ValidViaMcp", "status": "published" })
        )
        .is_ok(),
        "MCP rejected a valid create"
    );
    assert!(
        h.mcp_call("create_articles", &json!({ "status": "published" }))
            .is_err(),
        "MCP accepted a doc missing required title"
    );
}

/// Invariant 5b: the `unique` constraint on `title` is enforced identically —
/// a duplicate is rejected whether the original was written via either surface.
#[tokio::test]
async fn unique_constraint_is_enforced_across_grpc_lua_and_mcp() {
    let h = harness();

    // Seed a title via Lua; the duplicate must be rejected on BOTH surfaces.
    assert!(
        h.lua_create_ok(r#"{ title = "Duplicate" }"#),
        "initial create should succeed"
    );
    assert!(
        !h.grpc_create_ok(&[("title", "Duplicate")]).await,
        "gRPC allowed a duplicate unique title"
    );
    assert!(
        !h.lua_create_ok(r#"{ title = "Duplicate" }"#),
        "Lua allowed a duplicate unique title"
    );
    assert!(
        h.mcp_call("create_articles", &json!({ "title": "Duplicate" }))
            .is_err(),
        "MCP allowed a duplicate unique title"
    );
}
