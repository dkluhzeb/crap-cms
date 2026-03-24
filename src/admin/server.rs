//! Axum router setup, auth middleware, and admin server startup.

use std::{
    collections::HashMap, future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration,
};

use anyhow::Result;
use axum::{
    Json, Router,
    body::{self, Body},
    extract::{DefaultBodyLimit, State},
    http::{
        Method, Request, StatusCode,
        header::{self, HeaderName, HeaderValue},
    },
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{MethodRouter, get, post},
};
use hyper_util::{rt::TokioIo, server::conn::auto::Builder as AutoBuilder};
use serde_json::Value;
use subtle::ConstantTimeEq;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tower::Service;
use tower_http::{compression::CompressionLayer, trace::TraceLayer};

use crate::{
    admin::{
        AdminState, Translations,
        context::ContextBuilder,
        handlers::{
            auth as auth_handlers, collections, dashboard, events, globals, static_assets, uploads,
        },
        templates,
    },
    api::upload::upload_router,
    config::{CompressionMode, CrapConfig, LocaleConfig},
    core::{
        AuthUser, JwtSecret, Registry, Slug,
        auth::{self, ClaimsBuilder},
        collection::Auth as CollectionAuth,
        email::EmailRenderer,
        event::EventBus,
        rate_limit::LoginRateLimiter,
    },
    db::{DbConnection, DbPool, query},
    hooks::HookRunner,
    mcp::{
        McpServer,
        protocol::{INTERNAL_ERROR, JsonRpcError, JsonRpcRequest, JsonRpcResponse, PARSE_ERROR},
    },
};

/// Parameters for starting the admin HTTP server.
pub struct AdminStartParams {
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub hook_runner: HookRunner,
    pub jwt_secret: JwtSecret,
    pub event_bus: Option<EventBus>,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
}

impl AdminStartParams {
    /// Create a builder for `AdminStartParams`.
    pub fn builder() -> crate::admin::server_builder::AdminStartParamsBuilder {
        crate::admin::server_builder::AdminStartParamsBuilder::new()
    }
}

/// Start the admin HTTP server (Axum) with all routes, middleware, and static file serving.
// Excluded from coverage: async server startup orchestration (binds TCP listener, runs Axum server).
#[cfg(not(tarpaulin_include))]
pub async fn start(
    addr: &str,
    params: AdminStartParams,
    shutdown: CancellationToken,
) -> Result<()> {
    let AdminStartParams {
        config,
        config_dir,
        pool,
        registry,
        hook_runner,
        jwt_secret,
        event_bus,
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
    } = params;
    let translations = Arc::new(Translations::load(&config_dir));
    let handlebars =
        templates::create_handlebars(&config_dir, config.admin.dev_mode, translations.clone())?;
    let email_renderer = Arc::new(EmailRenderer::new(&config_dir)?);

    // Check if any auth collections exist
    let has_auth = registry
        .collections
        .values()
        .any(|d| d.is_auth_collection());

    let state = AdminState {
        config,
        config_dir: config_dir.clone(),
        pool,
        registry,
        handlebars,
        hook_runner,
        jwt_secret,
        email_renderer,
        event_bus,
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
        has_auth,
        translations,
        shutdown: shutdown.clone(),
    };

    let h2c_enabled = state.config.server.h2c;
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let shutdown_timeout = shutdown.clone();

    let server_future: Pin<Box<dyn Future<Output = Result<()>> + Send>> = if h2c_enabled {
        tracing::info!("Admin server: h2c (HTTP/2 cleartext) enabled");
        Box::pin(serve_h2c(listener, app, shutdown))
    } else {
        Box::pin(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await?;
            Ok(())
        })
    };

    // Hard deadline: force-stop after 10s if graceful drain doesn't complete
    // (SSE streams and other long-lived connections may not close promptly)
    select! {
        result = server_future => { result?; }
        _ = async {
            shutdown_timeout.cancelled().await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        } => {
            tracing::warn!("Admin server: graceful shutdown timed out after 10s");
        }
    }

    Ok(())
}

/// Run the admin server with h2c (HTTP/2 cleartext) support.
/// Uses hyper-util's auto::Builder which negotiates HTTP/1.1 vs HTTP/2
/// on the same port. Reverse proxies can speak HTTP/2 to the backend
/// without TLS; browsers fall back to HTTP/1.1 gracefully.
#[cfg(not(tarpaulin_include))]
async fn serve_h2c(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: CancellationToken,
) -> Result<()> {
    loop {
        select! {
            result = listener.accept() => {
                let (socket, addr) = result?;
                let tower_service = app.clone();
                tokio::spawn(async move {
                    let hyper_service = hyper::service::service_fn(move |mut req| {
                        // Insert ConnectInfo so extractors can read the client address
                        // (axum::serve does this automatically; h2c needs it manually)
                        req.extensions_mut()
                            .insert(axum::extract::ConnectInfo(addr));
                        tower_service.clone().call(req)
                    });
                    let io = TokioIo::new(socket);
                    AutoBuilder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection_with_upgrades(io, hyper_service)
                        .await
                        .ok(); // Connection errors are expected (client disconnect)
                });
            }
            _ = shutdown.cancelled() => break,
        }
    }
    Ok(())
}

/// Build reusable method routers for collection and global endpoints.
#[cfg(not(tarpaulin_include))]
fn method_routers() -> (
    MethodRouter<AdminState>,
    MethodRouter<AdminState>,
    MethodRouter<AdminState>,
) {
    let slug = MethodRouter::new()
        .get(collections::list_items)
        .post(collections::create_action);
    let item = MethodRouter::new()
        .get(collections::edit_form)
        .post(collections::update_action)
        .put(collections::update_action)
        .delete(collections::delete_action);
    let globals = MethodRouter::new()
        .get(globals::edit_form)
        .post(globals::update_action);
    (slug, item, globals)
}

/// Assemble the protected admin routes (everything behind auth middleware).
#[cfg(not(tarpaulin_include))]
fn protected_routes(
    slug_methods: MethodRouter<AdminState>,
    item_methods: MethodRouter<AdminState>,
    globals_methods: MethodRouter<AdminState>,
) -> Router<AdminState> {
    Router::new()
        .route("/", get(dashboard::index))
        .route("/admin", get(dashboard::index))
        .route("/admin/collections", get(collections::list_collections))
        .route("/admin/collections/{slug}", slug_methods)
        .route(
            "/admin/collections/{slug}/create",
            get(collections::create_form),
        )
        .route("/admin/collections/{slug}/{id}", item_methods)
        .route(
            "/admin/collections/{slug}/{id}/delete",
            get(collections::delete_confirm),
        )
        .route(
            "/admin/collections/{slug}/{id}/versions",
            get(collections::list_versions_page),
        )
        .route(
            "/admin/collections/{slug}/{id}/versions/{version_id}/restore",
            get(collections::restore_confirm).post(collections::restore_version),
        )
        .route(
            "/admin/collections/{slug}/validate",
            post(collections::items::validate::validate_create),
        )
        .route(
            "/admin/collections/{slug}/{id}/validate",
            post(collections::items::validate::validate_update),
        )
        .route(
            "/admin/collections/{slug}/evaluate-conditions",
            post(collections::evaluate_conditions),
        )
        .route(
            "/admin/api/search/{slug}",
            get(collections::search_collection),
        )
        .route(
            "/admin/api/user-settings/{slug}",
            post(collections::save_user_settings),
        )
        .route("/admin/globals/{slug}", globals_methods)
        .route(
            "/admin/globals/{slug}/validate",
            post(globals::validate::validate_global),
        )
        .route(
            "/admin/globals/{slug}/versions",
            get(globals::list_versions_page),
        )
        .route(
            "/admin/globals/{slug}/versions/{version_id}/restore",
            get(globals::restore_confirm).post(globals::restore_version),
        )
        .route("/admin/events", get(events::sse_handler))
        .route(
            "/admin/api/session-refresh",
            post(auth_handlers::session_refresh),
        )
        .route("/admin/api/locale", post(auth_handlers::save_locale))
}

/// Build the full admin Axum router with all routes, middleware, and state.
/// Separated from `start()` so integration tests can construct the router
/// without binding to a TCP listener.
// Excluded from coverage: requires full AdminState (HookRunner with Lua VM, DB pool,
// Handlebars registry, etc). Tested indirectly through CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn build_router(state: AdminState) -> Router {
    let (slug_methods, item_methods, globals_methods) = method_routers();
    let protected = protected_routes(slug_methods, item_methods, globals_methods);

    // Apply auth middleware if auth collections exist OR require_auth is set
    let needs_auth_layer = state.has_auth || state.config.admin.require_auth;
    let protected = if needs_auth_layer {
        protected.layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
    } else {
        protected
    };

    let config_dir = &state.config_dir;

    // Mount MCP HTTP endpoint if enabled
    let mcp_route = if state.config.mcp.enabled && state.config.mcp.http {
        Some(post(mcp_http_handler))
    } else {
        None
    };

    let upload_api = upload_router(state.clone());

    let router = Router::new()
        .route("/health", get(health_liveness))
        .route("/ready", get(health_readiness))
        .route(
            "/admin/login",
            get(auth_handlers::login_page).post(auth_handlers::login_action),
        )
        .route(
            "/admin/logout",
            get(auth_handlers::logout_action).post(auth_handlers::logout_action),
        )
        .route(
            "/admin/forgot-password",
            get(auth_handlers::forgot_password_page).post(auth_handlers::forgot_password_action),
        )
        .route(
            "/admin/reset-password",
            get(auth_handlers::reset_password_page).post(auth_handlers::reset_password_action),
        )
        .route("/admin/verify-email", get(auth_handlers::verify_email))
        .merge(protected)
        .merge(if let Some(mcp) = mcp_route {
            Router::new().route("/mcp", mcp)
        } else {
            Router::new()
        })
        .nest("/api", upload_api)
        .nest_service("/static", static_assets::overlay_service(config_dir))
        .route(
            "/uploads/{collection_slug}/{filename}",
            get(uploads::serve_upload),
        )
        .layer(DefaultBodyLimit::max(
            (state.config.upload.max_file_size + 1024 * 1024) as usize,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf_middleware,
        ))
        .layer(middleware::from_fn(html_cache_control))
        .layer(middleware::from_fn(security_headers));

    // Add CORS layer if configured (runs before CSRF in request processing)
    let router = if let Some(cors) = state.config.cors.build_layer() {
        router.layer(cors)
    } else {
        router
    };

    // Add response compression if configured
    let router = match state.config.server.compression {
        CompressionMode::Off => router,
        CompressionMode::Gzip => {
            router.layer(CompressionLayer::new().no_br().no_deflate().no_zstd())
        }
        CompressionMode::Br => {
            router.layer(CompressionLayer::new().no_gzip().no_deflate().no_zstd())
        }
        CompressionMode::All => router.layer(CompressionLayer::new()),
    };

    // Request tracing: per-request spans with method, path, status, latency
    let router = router.layer(
        TraceLayer::new_for_http()
            .make_span_with(|req: &Request<_>| {
                let request_id = nanoid::nanoid!(12);
                tracing::info_span!(
                    "http",
                    method = %req.method(),
                    path = %req.uri().path(),
                    request_id = %request_id,
                )
            })
            .on_response(
                |resp: &axum::http::Response<_>, latency: Duration, _span: &tracing::Span| {
                    tracing::info!(
                        status = resp.status().as_u16(),
                        latency_ms = latency.as_millis(),
                        "response"
                    );
                },
            ),
    );

    router.with_state(state)
}

/// Liveness probe — always returns 200 OK.
async fn health_liveness() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe — returns 200 if DB pool is healthy, 503 otherwise.
async fn health_readiness(State(state): State<AdminState>) -> StatusCode {
    match state.pool.get() {
        Ok(conn) => match conn.query_one("SELECT 1", &[]) {
            Ok(_) => StatusCode::OK,
            Err(_) => StatusCode::SERVICE_UNAVAILABLE,
        },
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Security headers middleware — sets protective headers on every response.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );

    response
}

/// Cache-Control middleware — sets `no-store` on HTML responses to prevent
/// browsers from back/forward-caching stale admin pages after mutations.
/// Does not affect static files (CSS/JS/fonts) or uploaded files (images/PDFs)
/// since those have non-HTML content types.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn html_cache_control(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;

    if let Some(ct) = response.headers().get(header::CONTENT_TYPE)
        && ct.to_str().unwrap_or("").starts_with("text/html")
    {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }

    response
}

/// Validate CSRF token on a mutating request. Checks the `X-CSRF-Token` header
/// first, then falls back to the `_csrf` form field for URL-encoded bodies.
/// Returns the (possibly re-assembled) request on success, or a 403 response.
#[cfg(not(tarpaulin_include))]
async fn validate_csrf_mutation(
    request: Request<Body>,
    cookie_value: &str,
) -> Result<Request<Body>, Response> {
    // Check X-CSRF-Token header first (set by HTMX / JS)
    let header_token = request
        .headers()
        .get("X-CSRF-Token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some(ref ht) = header_token
        && bool::from(ht.as_bytes().ct_eq(cookie_value.as_bytes()))
    {
        return Ok(request);
    }

    // Fall back: check _csrf in URL-encoded form body
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if content_type.starts_with("application/x-www-form-urlencoded") {
        let (parts, body) = request.into_parts();
        let bytes = body::to_bytes(body, 2 * 1024 * 1024).await.map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                "CSRF validation failed: body read error",
            )
                .into_response()
        })?;

        let form_token = form_urlencoded::parse(&bytes)
            .find(|(k, _)| k == "_csrf")
            .map(|(_, v)| v.to_string());

        if let Some(ref ft) = form_token
            && bool::from(ft.as_bytes().ct_eq(cookie_value.as_bytes()))
        {
            return Ok(Request::from_parts(parts, Body::from(bytes)));
        }
    }

    Err((StatusCode::FORBIDDEN, "CSRF validation failed").into_response())
}

/// CSRF middleware — double-submit cookie pattern.
/// Sets `crap_csrf` cookie on GET responses (non-HttpOnly so JS can read it).
/// Validates `X-CSRF-Token` header or `_csrf` form field on POST/PUT/DELETE.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn csrf_middleware(
    State(state): State<AdminState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let dev_mode = state.config.admin.dev_mode;

    // Bearer-authenticated API clients can't use double-submit cookies.
    // CSRF protects browser sessions (cookies); Bearer tokens aren't auto-attached
    // by browsers, so CSRF is irrelevant for them.
    let has_bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer "));

    if has_bearer {
        return next.run(request).await;
    }

    let cookie_header = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let csrf_cookie = extract_cookie(&cookie_header, "crap_csrf").map(|s| s.to_string());

    // On mutating methods, validate CSRF token
    if matches!(
        method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    ) {
        let cookie_value = match &csrf_cookie {
            Some(v) if !v.is_empty() => v.as_str(),
            _ => {
                return (
                    StatusCode::FORBIDDEN,
                    "CSRF validation failed: no token cookie",
                )
                    .into_response();
            }
        };

        match validate_csrf_mutation(request, cookie_value).await {
            Ok(request) => {
                let mut response = next.run(request).await;
                ensure_csrf_cookie(&mut response, csrf_cookie.as_deref(), dev_mode);
                return response;
            }
            Err(response) => return response,
        }
    }

    // Non-mutating method — pass through and set cookie if needed
    let mut response = next.run(request).await;
    ensure_csrf_cookie(&mut response, csrf_cookie.as_deref(), dev_mode);
    response
}

/// Set the `crap_csrf` cookie on the response if not already present in the request.
/// Adds `Secure` flag in production mode (same as session cookies).
fn ensure_csrf_cookie(response: &mut Response, existing_cookie: Option<&str>, dev_mode: bool) {
    if existing_cookie.is_some() {
        return;
    }

    let token = nanoid::nanoid!(32);
    let secure = if dev_mode { "" } else { "; Secure" };
    let cookie = format!(
        "crap_csrf={}; Path=/; SameSite=Strict; Max-Age=86400{}",
        token, secure
    );

    if let Ok(value) = cookie.parse() {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

/// Validate JWT from `crap_session` cookie and optionally load the full user document.
#[cfg(not(tarpaulin_include))]
fn validate_jwt_and_load_user(
    state: &AdminState,
    cookie_header: &str,
) -> Option<(auth::Claims, Option<AuthUser>)> {
    let token = extract_cookie(cookie_header, "crap_session")?;
    let claims = auth::validate_token(token, state.jwt_secret.as_ref()).ok()?;
    let auth_user = load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale);
    Some((claims, auth_user))
}

/// Evaluate custom auth strategies in a blocking context (Lua + DB access).
/// Tries each strategy across all auth collections until one succeeds.
#[cfg(not(tarpaulin_include))]
fn try_strategy_auth(
    auth_defs: &[(Slug, CollectionAuth)],
    headers: &HashMap<String, String>,
    pool: &DbPool,
    hook_runner: &HookRunner,
) -> Option<auth::Claims> {
    let mut conn = pool.get().ok()?;
    let tx = conn.transaction().ok()?;
    let mut result = None;

    for (slug, auth_config) in auth_defs {
        if result.is_some() {
            break;
        }
        for strategy in &auth_config.strategies {
            match hook_runner.run_auth_strategy(&strategy.authenticate, slug, headers, &tx) {
                Ok(Some(user)) => {
                    let user_email = user
                        .fields
                        .get("email")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let expiry = auth_config.token_expiry;
                    let claims = ClaimsBuilder::new(user.id.clone(), slug.clone())
                        .email(user_email)
                        .exp((chrono::Utc::now().timestamp() as u64) + expiry)
                        .build();
                    result = Some(claims);
                    break;
                }
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        "Auth strategy '{}' error for {}: {}",
                        strategy.name,
                        slug,
                        e
                    );
                    continue;
                }
            }
        }
    }

    // Read-only access check — commit result is irrelevant, rollback on drop is safe
    let _ = tx.commit();
    result
}

/// Build a login redirect response (HTMX-aware: uses HX-Redirect instead of 302).
#[cfg(not(tarpaulin_include))]
fn login_redirect(request: &Request<Body>) -> Response {
    let is_htmx = request.headers().get("HX-Request").is_some();

    if is_htmx {
        Response::builder()
            .status(StatusCode::OK)
            .header("HX-Redirect", "/admin/login")
            .body(Body::empty())
            .expect("static response builder")
    } else {
        Redirect::to("/admin/login").into_response()
    }
}

/// Insert authenticated claims (and optionally user) into request extensions,
/// checking the admin access gate first. Returns a gate-denied response if blocked.
#[cfg(not(tarpaulin_include))]
async fn apply_auth_to_request(
    state: &AdminState,
    request: &mut Request<Body>,
    claims: auth::Claims,
    auth_user: Option<AuthUser>,
) -> Option<Response> {
    if let Some(user) = auth_user {
        if let Some(response) = check_admin_gate(state, &user).await {
            return Some(response);
        }
        request.extensions_mut().insert(user);
    }
    request.extensions_mut().insert(claims);
    None
}

/// Auth middleware — extracts JWT from `crap_session` cookie, validates it,
/// and stores `Claims` in request extensions. If JWT is invalid/missing,
/// tries custom auth strategies before redirecting to login.
///
/// Also enforces two admin access gates:
/// 1. If no auth collections exist and `require_auth` is true, shows "setup required" page.
/// 2. If `admin.access` is configured, checks the Lua function after authentication.
// Excluded from coverage: async Axum middleware requiring full server state (pool, registry,
// HookRunner, JWT secret) and spawned blocking tasks for Lua auth strategies.
#[cfg(not(tarpaulin_include))]
async fn auth_middleware(
    State(state): State<AdminState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    // Gate 1: No auth collections but require_auth is on → setup required
    if !state.has_auth && state.config.admin.require_auth {
        return auth_required_response(&state);
    }

    let cookie_header = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Fast path: valid JWT cookie
    if let Some((claims, auth_user)) = validate_jwt_and_load_user(&state, cookie_header) {
        if let Some(response) = apply_auth_to_request(&state, &mut request, claims, auth_user).await
        {
            return response;
        }
        return next.run(request).await;
    }

    // Collect custom strategies from all auth collections
    let auth_defs: Vec<_> = state
        .registry
        .collections
        .values()
        .filter(|d| d.is_auth_collection())
        .filter(|d| {
            d.auth
                .as_ref()
                .map(|a| !a.strategies.is_empty())
                .unwrap_or(false)
        })
        .map(|d| (d.slug.clone(), d.auth.clone().expect("guarded by filter")))
        .collect();

    if !auth_defs.is_empty() {
        let headers: HashMap<String, String> = request
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_string(), v.to_string()))
            })
            .collect();

        let pool = state.pool.clone();
        let hook_runner = state.hook_runner.clone();

        let strategy_result = tokio::task::spawn_blocking(move || {
            try_strategy_auth(&auth_defs, &headers, &pool, &hook_runner)
        })
        .await;

        if let Ok(Some(claims)) = strategy_result {
            let auth_user =
                load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale);
            if let Some(response) =
                apply_auth_to_request(&state, &mut request, claims, auth_user).await
            {
                return response;
            }
            return next.run(request).await;
        }
    }

    login_redirect(&request)
}

/// Gate 2: Check `admin.access` Lua function. Returns a 403 response if the user
/// is denied, or None if access is allowed (or no access function is configured).
// Excluded from coverage: requires HookRunner + DB pool for Lua access check.
#[cfg(not(tarpaulin_include))]
async fn check_admin_gate(state: &AdminState, auth_user: &AuthUser) -> Option<Response> {
    let access_ref = state.config.admin.access.as_deref()?;
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let user_doc = auth_user.user_doc.clone();
    let access_ref = access_ref.to_string();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get().ok()?;
        Some(hook_runner.check_access(Some(&access_ref), Some(&user_doc), None, None, &conn))
    })
    .await;

    match result {
        Ok(Some(Ok(query::AccessResult::Denied))) => Some(admin_denied_response(state)),
        Ok(Some(Err(e))) => {
            tracing::error!("admin.access check failed: {}", e);
            Some(admin_denied_response(state))
        }
        _ => None, // Allowed or Constrained — pass through
    }
}

/// Render the "setup required" page (no auth collection exists, require_auth is on).
fn auth_required_response(state: &AdminState) -> Response {
    let data = ContextBuilder::auth(state).build();

    match state.render("errors/auth_required", &data) {
        Ok(html) => (StatusCode::SERVICE_UNAVAILABLE, Html(html)).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Setup required: no auth collection configured",
        )
            .into_response(),
    }
}

/// Render the "access denied" page (user authenticated but not authorized for admin).
fn admin_denied_response(state: &AdminState) -> Response {
    let data = ContextBuilder::auth(state).build();

    match state.render("errors/admin_denied", &data) {
        Ok(html) => (StatusCode::FORBIDDEN, Html(html)).into_response(),
        Err(_) => (StatusCode::FORBIDDEN, "Access denied").into_response(),
    }
}

/// Load the full user document for an authenticated user.
/// Returns None if the user can't be found (e.g., deleted since JWT was issued).
/// Also loads `ui_locale` from `_crap_user_settings`.
// Excluded from coverage: requires full DB pool + SharedRegistry with auth collection definitions.
// Tested indirectly through integration tests (admin login flow).
#[cfg(not(tarpaulin_include))]
pub(crate) fn load_auth_user(
    pool: &DbPool,
    registry: &Registry,
    claims: &auth::Claims,
    locale_config: &LocaleConfig,
) -> Option<AuthUser> {
    let def = registry.get_collection(&claims.collection)?.clone();
    let locale_ctx = query::LocaleContext::from_locale_string(None, locale_config);

    let conn = pool.get().ok()?;

    let doc = query::find_by_id(
        &conn,
        &claims.collection,
        &def,
        &claims.sub,
        locale_ctx.as_ref(),
    )
    .ok()??;

    // Load ui_locale from _crap_user_settings
    let ui_locale = query::get_user_settings(&conn, &claims.sub)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("ui_locale")
                .and_then(|l| l.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| locale_config.default_locale.clone());

    let mut auth = AuthUser::new(claims.clone(), doc);
    auth.ui_locale = ui_locale;

    Some(auth)
}

/// Extract a named cookie value from a Cookie header string.
pub(crate) fn extract_cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    for part in header.split(';') {
        let trimmed = part.trim();

        if let Some(value) = trimmed.strip_prefix(name)
            && let Some(value) = value.strip_prefix('=')
        {
            return Some(value);
        }
    }

    None
}

/// MCP HTTP transport handler — receives JSON-RPC 2.0 over POST /mcp.
/// Optionally validates API key from Authorization header.
// Excluded from coverage: async Axum handler requiring full server state.
#[cfg(not(tarpaulin_include))]
async fn mcp_http_handler(State(state): State<AdminState>, request: Request<Body>) -> Response {
    // API key auth — constant-time comparison to prevent timing attacks
    if !state.config.mcp.api_key.is_empty() {
        let auth_header = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let expected = format!("Bearer {}", state.config.mcp.api_key);
        let is_valid = auth_header.as_bytes().ct_eq(expected.as_bytes());

        if !bool::from(is_valid) {
            return (StatusCode::UNAUTHORIZED, "Invalid or missing API key").into_response();
        }
    }

    let body_bytes = match body::to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "Request body too large").into_response(),
    };

    let rpc_request: JsonRpcRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            let error_resp = JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: None,
                result: None,
                error: Some(JsonRpcError {
                    code: PARSE_ERROR,
                    message: format!("Parse error: {}", e),
                    data: None,
                }),
            };
            return Json(error_resp).into_response();
        }
    };

    let server = McpServer {
        pool: state.pool.clone(),
        registry: state.registry.clone(),
        runner: state.hook_runner.clone(),
        config: state.config.clone(),
        config_dir: state.config_dir.clone(),
    };

    // Run handle_message in spawn_blocking — it does DB queries, Lua hooks, and filesystem I/O
    let response =
        match tokio::task::spawn_blocking(move || server.handle_message(rpc_request)).await {
            Ok(resp) => resp,
            Err(_) => JsonRpcResponse::error(None, INTERNAL_ERROR, "Internal error"),
        };

    // Notifications must not receive a response per JSON-RPC spec
    if response.id.is_none() && response.result.is_none() && response.error.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    Json(response).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_cookie_single() {
        assert_eq!(
            extract_cookie("crap_session=abc123", "crap_session"),
            Some("abc123")
        );
    }

    #[test]
    fn extract_cookie_multiple() {
        assert_eq!(
            extract_cookie(
                "other=val; crap_session=token123; another=x",
                "crap_session"
            ),
            Some("token123")
        );
    }

    #[test]
    fn extract_cookie_missing() {
        assert_eq!(extract_cookie("other=val; foo=bar", "crap_session"), None);
    }

    #[test]
    fn extract_cookie_empty_header() {
        assert_eq!(extract_cookie("", "crap_session"), None);
    }

    #[test]
    fn extract_cookie_prefix_match_does_not_confuse() {
        // "crap_session_old" should NOT match "crap_session"
        assert_eq!(extract_cookie("crap_session_old=bad", "crap_session"), None);
    }

    #[test]
    fn extract_cookie_exact_name_with_similar_prefix() {
        // Both "crap_session_old" and "crap_session" present — should get correct one
        assert_eq!(
            extract_cookie("crap_session_old=bad; crap_session=good", "crap_session"),
            Some("good")
        );
    }

    #[test]
    fn extract_cookie_value_with_equals() {
        // Cookie values can contain = (like base64)
        assert_eq!(
            extract_cookie("token=abc=def==", "token"),
            Some("abc=def==")
        );
    }
}
