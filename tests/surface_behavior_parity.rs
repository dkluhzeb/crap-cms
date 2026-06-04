//! Cross-surface **behavioral** parity (Phase 1: gRPC ↔ Lua).
//!
//! The routing guard proves surfaces delegate to the service layer, and the
//! capability guard proves every op exists on every surface. This suite proves
//! the surfaces actually *behave the same*: for the same data and query, the
//! gRPC and Lua adapters must return identical results. That's the layer that
//! would have caught "count disagrees with find" / "count ignores a filter on
//! one surface" — adapter-level drift the source-scan guards can't see.
//!
//! Both surfaces are driven in-process over a **shared registry + pool**, so
//! a write through one is visible to the other. (MCP's `exec_*` tools are
//! `pub(in crate::mcp::tools)` and unreachable from an external test; they're
//! covered by their colocated tests + the capability guard. Admin HTTP is a
//! later phase.)

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

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

/// Build a prost `Struct` from string key/value pairs (the gRPC `data` field).
fn make_struct(pairs: &[(&str, &str)]) -> prost_types::Struct {
    use prost_types::{Value, value::Kind};
    let fields = pairs
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_string(),
                Value {
                    kind: Some(Kind::StringValue((*v).to_string())),
                },
            )
        })
        .collect();
    prost_types::Struct { fields }
}

/// A gRPC `ContentService` and a Lua `HookRunner` over one shared registry+pool.
struct ParityHarness {
    _tmp: tempfile::TempDir,
    service: ContentService,
    lua: HookRunner,
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

    fn lua_find_total(&self, where_frag: &str) -> i64 {
        self.lua_eval(&format!(
            "return tostring(crap.collections.find(\"articles\", {{ {where_frag} }}).pagination.totalDocs)"
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
async fn count_and_find_totals_agree_across_grpc_and_lua() {
    let h = harness();
    h.seed_two_published_one_draft();

    let gc = h.grpc_count(None).await;
    let gf = h.grpc_find_total(None).await;
    let lc = h.lua_count("");
    let lf = h.lua_find_total("");

    assert_eq!(
        (gc, gf, lc, lf),
        (3, 3, 3, 3),
        "count/find totals must agree across surfaces \
         (grpc_count={gc}, grpc_find={gf}, lua_count={lc}, lua_find={lf})"
    );
}

/// Invariant 1b: a `where` filter is honored identically by `count` and `find`
/// on both surfaces — catches "count ignores the filter on one surface".
#[tokio::test]
async fn filter_is_honored_identically_by_count_and_find_on_both_surfaces() {
    let h = harness();
    h.seed_two_published_one_draft();

    let gc = h.grpc_count(Some(r#"{"status":"published"}"#)).await;
    let gf = h.grpc_find_total(Some(r#"{"status":"published"}"#)).await;
    let lc = h.lua_count(r#"where = { status = "published" }"#);
    let lf = h.lua_find_total(r#"where = { status = "published" }"#);

    assert_eq!(
        (gc, gf, lc, lf),
        (2, 2, 2, 2),
        "filtered count/find must agree across surfaces \
         (grpc_count={gc}, grpc_find={gf}, lua_count={lc}, lua_find={lf})"
    );
}

/// Invariant 5: validation must reject (or accept) identically on both
/// surfaces — a constraint enforced by one adapter but not the other would let
/// invalid data in through that surface. Uses the `title` field's
/// `required` + `unique` constraints. (Distinct valid titles per surface
/// because `title` is unique.)
#[tokio::test]
async fn validation_rejection_is_consistent_across_grpc_and_lua() {
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
}

/// Invariant 5b: the `unique` constraint on `title` is enforced identically —
/// a duplicate is rejected whether the original was written via either surface.
#[tokio::test]
async fn unique_constraint_is_enforced_across_grpc_and_lua() {
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
}
