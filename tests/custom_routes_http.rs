//! Live-HTTP integration tests for custom routes: drives real requests through
//! the assembled admin router (`build_router` → `dispatch_custom_route`),
//! covering status codes, headers/cookies, rate limiting, the access gate,
//! disabled routes, CSRF, and the body-size limit.

#![allow(clippy::missing_panics_doc, clippy::too_many_lines)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crap_cms::admin::AdminState;
use crap_cms::admin::server::build_router;
use crap_cms::admin::templates;
use crap_cms::admin::translations::Translations;
use crap_cms::config::CrapConfig;
use crap_cms::core::Registry;
use crap_cms::core::rate_limit::LoginRateLimiter;
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;

const ROUTE_FILES: &[(&str, &str)] = &[
    (
        "init.lua",
        r#"
        crap.routes.register({ path = "/echo", method = "GET", handler = "routes.echo" })
        crap.routes.register({ path = "/limited", method = "GET", handler = "routes.echo", rate_limit = { max = 1, window = 60 } })
        crap.routes.register({ path = "/secret", method = "GET", handler = "routes.echo", access = "access.deny" })
        crap.routes.register({ path = "/off", method = "GET", handler = "routes.echo", access = false })
        crap.routes.register({ path = "/csrf", method = "POST", handler = "routes.echo", csrf = true })
        crap.routes.register({ path = "/tiny", method = "POST", handler = "routes.echo", max_body = 10 })
        "#,
    ),
    (
        "routes/echo.lua",
        r#"return function(ctx)
            return {
                json = { ok = true, method = ctx.method },
                headers = { ["x-custom"] = "yes" },
                cookies = { { name = "rid", value = "abc", http_only = true, path = "/" } },
            }
        end"#,
    ),
    ("access/deny.lua", "return function(ctx) return false end"),
];

fn setup() -> (tempfile::TempDir, axum::Router) {
    let tmp = tempfile::tempdir().expect("tempdir");
    for (rel, contents) in ROUTE_FILES {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.admin.require_auth = false;

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");
    let registry = Registry::snapshot(&Registry::shared());
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

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
        db_pool,
        registry,
        hook_runner,
        storage,
        token_provider,
        &config,
        tmp.path(),
    );

    let state = AdminState {
        mcp_sessions: Arc::default(),
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        handlebars,
        jwt_secret: "test-jwt-secret".into(),
        email_provider: crap_cms::core::email::create_email_provider(
            &crap_cms::config::EmailConfig::default(),
        )
        .unwrap(),
        login_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_login_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: Arc::new(LoginRateLimiter::new(3, 900)),
        ip_forgot_password_limiter: Arc::new(LoginRateLimiter::new(20, 900)),
        mfa_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        has_auth: false,
        translations,
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
        password_provider: Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: crap_cms::admin::custom_pages::CustomPageRegistry::default(),
    };

    (tmp, build_router(state))
}

/// Fire one request through the router.
async fn send(router: &axum::Router, req: Request<Body>) -> axum::http::Response<Body> {
    router.clone().oneshot(req).await.unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, body: Body) -> Request<Body> {
    Request::post(path)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn public_route_returns_json_with_header_and_cookie() {
    let (_tmp, router) = setup();
    let resp = send(&router, get("/echo")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("x-custom").map(|v| v.to_str().unwrap()),
        Some("yes")
    );
    let set_cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(set_cookie.contains("rid=abc"), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["method"], serde_json::json!("GET"));
}

#[tokio::test]
async fn wrong_method_is_405() {
    let (_tmp, router) = setup();
    let resp = send(&router, post("/echo", Body::empty())).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn rate_limit_returns_429_after_threshold() {
    let (_tmp, router) = setup();
    // max = 1: first allowed, second blocked.
    assert_eq!(
        send(&router, get("/limited")).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&router, get("/limited")).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn access_gate_denies_with_403() {
    let (_tmp, router) = setup();
    assert_eq!(
        send(&router, get("/secret")).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn disabled_route_is_404() {
    let (_tmp, router) = setup();
    assert_eq!(
        send(&router, get("/off")).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn unregistered_path_is_404() {
    let (_tmp, router) = setup();
    assert_eq!(
        send(&router, get("/nope")).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn csrf_route_rejects_missing_token() {
    let (_tmp, router) = setup();
    // POST with csrf=true and no token cookie/header → 403.
    assert_eq!(
        send(&router, post("/csrf", Body::empty())).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn body_over_max_body_is_413() {
    let (_tmp, router) = setup();
    // /tiny has max_body = 10; send 100 bytes.
    let resp = send(&router, post("/tiny", Body::from(vec![b'x'; 100]))).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
