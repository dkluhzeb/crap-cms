//! Integration tests for MCP HTTP session tracking (`Mcp-Session-Id`):
//! `initialize` opens a tracked session returned via the response header,
//! later requests resolve their audit identity from it, and DELETE
//! terminates it. Untracked requests keep working (identity is
//! audit-only; the API key authenticates every request).

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

use std::sync::Arc;

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::admin::AdminState;
use crap_cms::admin::mcp_sessions::McpSessions;
use crap_cms::admin::server::build_router;
use crap_cms::admin::templates;
use crap_cms::admin::translations::Translations;
use crap_cms::config::CrapConfig;
use crap_cms::core::Registry;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

struct TestApp {
    _tmp: tempfile::TempDir,
    router: axum::Router,
    sessions: Arc<McpSessions>,
}

fn posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    def
}

fn setup() -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.mcp.enabled = true;
    config.mcp.http = true;
    config.mcp.api_key = API_KEY.into();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(posts_def());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("handlebars");
    let storage = crap_cms::core::upload::create_storage(
        tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();
    let token_provider: crap_cms::core::SharedTokenProvider = Arc::new(
        crap_cms::core::auth::JwtTokenProvider::new("test-jwt-secret"),
    );
    let infra = crap_cms::admin::test_support::test_infra(
        db_pool.clone(),
        Arc::clone(&registry),
        hook_runner,
        storage,
        token_provider,
        &config,
        tmp.path(),
    );

    let sessions: Arc<McpSessions> = Arc::default();

    let state = AdminState {
        mcp_sessions: Arc::clone(&sessions),
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        handlebars,
        jwt_secret: "test-jwt-secret".into(),
        email_provider: crap_cms::core::email::create_email_provider(
            &crap_cms::config::EmailConfig::default(),
        )
        .unwrap(),
        login_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        ip_login_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            3, 900,
        )),
        ip_forgot_password_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )),
        mfa_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(20, 300)),
        has_auth: false,
        translations,
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
        password_provider: Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: crap_cms::admin::custom_pages::CustomPageRegistry::default(),
    };

    TestApp {
        _tmp: tmp,
        router: build_router(state),
        sessions,
    }
}

fn mcp_request(method: &str, session: Option<&str>, body: String) -> Request<Body> {
    let mut builder = Request::builder()
        .uri("/mcp")
        .method(method)
        .header("authorization", format!("Bearer {API_KEY}"))
        .header("content-type", "application/json");

    if let Some(id) = session {
        builder = builder.header("mcp-session-id", id);
    }

    let mut request = builder.body(Body::from(body)).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    request
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test-client","version":"1.0"}}}"#;

/// `initialize` opens a tracked session: the response carries
/// `Mcp-Session-Id`, and the store maps it to the announced client name.
#[tokio::test]
async fn initialize_opens_a_tracked_session() {
    let app = setup();

    let resp = app
        .router
        .clone()
        .oneshot(mcp_request("POST", None, INITIALIZE.to_string()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("initialize must return Mcp-Session-Id")
        .to_string();

    assert_eq!(
        app.sessions.lookup_touch(&session_id).as_deref(),
        Some("test-client"),
        "the session maps to the announced client name"
    );
}

/// A request without (or with an unknown) session id keeps working —
/// tracking is audit-only, never an auth gate.
#[tokio::test]
async fn untracked_requests_still_work() {
    let app = setup();
    let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string();

    for session in [None, Some("unknown-session-id")] {
        let resp = app
            .router
            .clone()
            .oneshot(mcp_request("POST", session, body.clone()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

/// DELETE terminates the session: 204 while it exists, 404 after, 400
/// without the header, and the API key is still required.
#[tokio::test]
async fn delete_terminates_the_session() {
    let app = setup();

    let resp = app
        .router
        .clone()
        .oneshot(mcp_request("POST", None, INITIALIZE.to_string()))
        .await
        .unwrap();
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();

    let resp = app
        .router
        .clone()
        .oneshot(mcp_request("DELETE", Some(&session_id), String::new()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .router
        .clone()
        .oneshot(mcp_request("DELETE", Some(&session_id), String::new()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "already terminated");

    let resp = app
        .router
        .clone()
        .oneshot(mcp_request("DELETE", None, String::new()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "header required");

    // Wrong key: rejected before touching the store.
    let resp = app
        .router
        .clone()
        .oneshot({
            let mut r = Request::builder()
                .uri("/mcp")
                .method("DELETE")
                .header("authorization", "Bearer wrong")
                .header("mcp-session-id", "x")
                .body(Body::empty())
                .unwrap();
            r.extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
            r
        })
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::NO_CONTENT);
    assert_ne!(resp.status(), StatusCode::NOT_FOUND);
}
