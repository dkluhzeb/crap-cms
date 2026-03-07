//! Axum router setup, auth middleware, and admin server startup.

use anyhow::Result;
use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    middleware::{self, Next},
    response::{IntoResponse, Redirect},
    routing::{get, post},
};
use axum::routing::MethodRouter;
use axum::http::{Method, StatusCode};
use std::path::PathBuf;

use crate::config::{CrapConfig, CompressionMode};
use crate::core::Registry;
use crate::core::auth::{self, AuthUser};
use crate::core::event::EventBus;
use crate::db::DbPool;
use crate::db::query;
use crate::hooks::lifecycle::HookRunner;
use super::AdminState;
use super::handlers::{auth as auth_handlers, dashboard, collections, globals, static_assets, uploads, events};

/// Start the admin HTTP server (Axum) with all routes, middleware, and static file serving.
// Excluded from coverage: async server startup orchestration (binds TCP listener, runs Axum server).
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub async fn start(
    addr: &str,
    config: CrapConfig,
    config_dir: PathBuf,
    pool: DbPool,
    registry: std::sync::Arc<Registry>,
    hook_runner: HookRunner,
    jwt_secret: String,
    event_bus: Option<EventBus>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let translations = std::sync::Arc::new(
        super::translations::Translations::load(&config_dir)
    );
    let handlebars = super::templates::create_handlebars(&config_dir, config.admin.dev_mode, translations.clone())?;
    let email_renderer = std::sync::Arc::new(
        crate::core::email::EmailRenderer::new(&config_dir)?
    );

    // Check if any auth collections exist
    let has_auth = registry.collections.values().any(|d| d.is_auth_collection());

    let login_limiter = std::sync::Arc::new(
        crate::core::rate_limit::LoginRateLimiter::new(
            config.auth.max_login_attempts,
            config.auth.login_lockout_seconds,
        )
    );
    let forgot_password_limiter = std::sync::Arc::new(
        crate::core::rate_limit::LoginRateLimiter::new(
            config.auth.max_forgot_password_attempts,
            config.auth.forgot_password_window_seconds,
        )
    );

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
        forgot_password_limiter,
        has_auth,
        translations,
        shutdown: shutdown.clone(),
    };

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let shutdown_timeout = shutdown.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned());

    // Hard deadline: force-stop after 10s if graceful drain doesn't complete
    // (SSE streams and other long-lived connections may not close promptly)
    tokio::select! {
        result = server => { result?; }
        _ = async {
            shutdown_timeout.cancelled().await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        } => {
            tracing::warn!("Admin server: graceful shutdown timed out after 10s");
        }
    }

    Ok(())
}

/// Build the full admin Axum router with all routes, middleware, and state.
/// Separated from `start()` so integration tests can construct the router
/// without binding to a TCP listener.
// Excluded from coverage: requires full AdminState (HookRunner with Lua VM, DB pool,
// Handlebars registry, etc). Tested indirectly through CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn build_router(state: AdminState) -> Router {
    let has_auth = state.has_auth;
    // Build method routers explicitly to handle multiple methods on same path
    let slug_methods: MethodRouter<AdminState> = MethodRouter::new()
        .get(collections::list_items)
        .post(collections::create_action);

    let item_methods: MethodRouter<AdminState> = MethodRouter::new()
        .get(collections::edit_form)
        .post(collections::update_action_post)
        .put(collections::update_action_post)
        .delete(collections::delete_action_simple);

    let globals_methods: MethodRouter<AdminState> = MethodRouter::new()
        .get(globals::edit_form)
        .post(globals::update_action);

    // Protected routes (everything behind /admin except login/logout)
    let protected = Router::new()
        .route("/", get(dashboard::index))
        .route("/admin", get(dashboard::index))
        .route("/admin/collections", get(collections::list_collections))
        .route("/admin/collections/{slug}", slug_methods)
        .route("/admin/collections/{slug}/create", get(collections::create_form))
        .route("/admin/collections/{slug}/{id}", item_methods)
        .route("/admin/collections/{slug}/{id}/delete", get(collections::delete_confirm))
        .route("/admin/collections/{slug}/{id}/versions", get(collections::list_versions_page))
        .route("/admin/collections/{slug}/{id}/versions/{version_id}/restore", post(collections::restore_version))
        .route("/admin/collections/{slug}/evaluate-conditions", post(collections::evaluate_conditions))
        .route("/admin/api/search/{slug}", get(collections::search_collection))
        .route("/admin/api/user-settings/{slug}", post(collections::save_user_settings))
        .route("/admin/globals/{slug}", globals_methods)
        .route("/admin/globals/{slug}/versions", get(globals::list_versions_page))
        .route("/admin/globals/{slug}/versions/{version_id}/restore", post(globals::restore_version))
        .route("/admin/events", get(events::sse_handler))
        .route("/admin/api/session-refresh", post(auth_handlers::session_refresh))
        .route("/admin/api/locale", post(auth_handlers::save_locale));

    // Apply auth middleware if auth collections exist OR require_auth is set
    let needs_auth_layer = has_auth || state.config.admin.require_auth;
    let protected = if needs_auth_layer {
        protected.layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
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

    let upload_api = crate::api::upload::upload_router(state.clone());

    let router = Router::new()
        .route("/health", get(health_liveness))
        .route("/ready", get(health_readiness))
        .route("/admin/login", get(auth_handlers::login_page).post(auth_handlers::login_action))
        .route("/admin/logout", get(auth_handlers::logout_action).post(auth_handlers::logout_action))
        .route("/admin/forgot-password", get(auth_handlers::forgot_password_page).post(auth_handlers::forgot_password_action))
        .route("/admin/reset-password", get(auth_handlers::reset_password_page).post(auth_handlers::reset_password_action))
        .route("/admin/verify-email", get(auth_handlers::verify_email))
        .merge(protected)
        .merge(if let Some(mcp) = mcp_route {
            Router::new().route("/mcp", mcp)
        } else {
            Router::new()
        })
        .nest("/api", upload_api)
        .nest_service("/static", static_assets::overlay_service(config_dir))
        .route("/uploads/{collection_slug}/{filename}", get(uploads::serve_upload))
        .layer(DefaultBodyLimit::max((state.config.upload.max_file_size + 1024 * 1024) as usize))
        .layer(middleware::from_fn_with_state(state.clone(), csrf_middleware))
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
        CompressionMode::Gzip => router.layer(
            tower_http::compression::CompressionLayer::new()
                .no_br().no_deflate().no_zstd()
        ),
        CompressionMode::Br => router.layer(
            tower_http::compression::CompressionLayer::new()
                .no_gzip().no_deflate().no_zstd()
        ),
        CompressionMode::All => router.layer(
            tower_http::compression::CompressionLayer::new()
        ),
    };

    // Request tracing: per-request spans with method, path, status, latency
    let router = router.layer(
        tower_http::trace::TraceLayer::new_for_http()
            .make_span_with(|req: &axum::http::Request<_>| {
                let request_id = nanoid::nanoid!(12);
                tracing::info_span!(
                    "http",
                    method = %req.method(),
                    path = %req.uri().path(),
                    request_id = %request_id,
                )
            })
            .on_response(
                |resp: &axum::http::Response<_>, latency: std::time::Duration, _span: &tracing::Span| {
                    tracing::info!(status = resp.status().as_u16(), latency_ms = latency.as_millis(), "response");
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
        Ok(conn) => {
            match conn.query_row("SELECT 1", [], |_| Ok(())) {
                Ok(()) => StatusCode::OK,
                Err(_) => StatusCode::SERVICE_UNAVAILABLE,
            }
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Security headers middleware — sets protective headers on every response.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn security_headers(
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        axum::http::HeaderName::from_static("x-frame-options"),
        axum::http::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("x-content-type-options"),
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("referrer-policy"),
        axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("permissions-policy"),
        axum::http::HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

/// Cache-Control middleware — sets `no-store` on HTML responses to prevent
/// browsers from back/forward-caching stale admin pages after mutations.
/// Does not affect static files (CSS/JS/fonts) or uploaded files (images/PDFs)
/// since those have non-HTML content types.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn html_cache_control(
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    if let Some(ct) = response.headers().get(axum::http::header::CONTENT_TYPE) {
        if ct.to_str().unwrap_or("").starts_with("text/html") {
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
        }
    }
    response
}

/// CSRF middleware — double-submit cookie pattern.
/// Sets `crap_csrf` cookie on GET responses (non-HttpOnly so JS can read it).
/// Validates `X-CSRF-Token` header or `_csrf` form field on POST/PUT/DELETE.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn csrf_middleware(
    State(state): State<AdminState>,
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let dev_mode = state.config.admin.dev_mode;
    let cookie_header = request.headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let csrf_cookie = extract_cookie(&cookie_header, "crap_csrf")
        .map(|s| s.to_string());

    // On mutating methods, validate CSRF token
    if matches!(method, Method::POST | Method::PUT | Method::DELETE) {
        let cookie_value = match &csrf_cookie {
            Some(v) if !v.is_empty() => v.as_str(),
            _ => {
                return (StatusCode::FORBIDDEN, "CSRF validation failed: no token cookie").into_response();
            }
        };

        // Check X-CSRF-Token header first (set by HTMX / JS)
        let header_token = request.headers()
            .get("X-CSRF-Token")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if let Some(ref ht) = header_token {
            if ht == cookie_value {
                // Header matches — proceed
                let mut response = next.run(request).await;
                ensure_csrf_cookie(&mut response, csrf_cookie.as_deref(), dev_mode);
                return response;
            }
        }

        // Fall back: check _csrf in URL-encoded form body
        let content_type = request.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if content_type.starts_with("application/x-www-form-urlencoded") {
            let (parts, body) = request.into_parts();
            let bytes = match axum::body::to_bytes(body, 2 * 1024 * 1024).await {
                Ok(b) => b,
                Err(_) => {
                    return (StatusCode::FORBIDDEN, "CSRF validation failed: body read error").into_response();
                }
            };

            let form_token = form_urlencoded::parse(&bytes)
                .find(|(k, _)| k == "_csrf")
                .map(|(_, v)| v.to_string());

            if let Some(ref ft) = form_token {
                if ft == cookie_value {
                    // Form field matches — reconstruct request and proceed
                    let request = axum::http::Request::from_parts(parts, axum::body::Body::from(bytes));
                    let mut response = next.run(request).await;
                    ensure_csrf_cookie(&mut response, csrf_cookie.as_deref(), dev_mode);
                    return response;
                }
            }
        }

        return (StatusCode::FORBIDDEN, "CSRF validation failed").into_response();
    }

    // Non-mutating method — pass through and set cookie if needed
    let mut response = next.run(request).await;
    ensure_csrf_cookie(&mut response, csrf_cookie.as_deref(), dev_mode);
    response
}

/// Set the `crap_csrf` cookie on the response if not already present in the request.
/// Adds `Secure` flag in production mode (same as session cookies).
fn ensure_csrf_cookie(
    response: &mut axum::response::Response,
    existing_cookie: Option<&str>,
    dev_mode: bool,
) {
    if existing_cookie.is_some() {
        return;
    }
    let token = nanoid::nanoid!(32);
    let secure = if dev_mode { "" } else { "; Secure" };
    let cookie = format!("crap_csrf={}; Path=/; SameSite=Strict; Max-Age=86400{}", token, secure);
    if let Ok(value) = cookie.parse() {
        response.headers_mut().append(axum::http::header::SET_COOKIE, value);
    }
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
    mut request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    // Gate 1: No auth collections but require_auth is on → setup required
    if !state.has_auth && state.config.admin.require_auth {
        return auth_required_response(&state);
    }

    // If no auth collections and require_auth is false, this middleware
    // wouldn't be applied (needs_auth_layer is false), so we're safe to
    // proceed assuming auth collections exist from here.

    let cookie_header = request.headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Fast path: valid JWT cookie
    let token = extract_cookie(cookie_header, "crap_session");
    if let Some(t) = token {
        if let Ok(claims) = auth::validate_token(t, &state.jwt_secret) {
            // Try to load full user document for access control
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale) {
                // Gate 2: Check admin.access Lua function
                if let Some(response) = check_admin_gate(&state, &auth_user).await {
                    return response;
                }
                request.extensions_mut().insert(auth_user);
            }
            request.extensions_mut().insert(claims);
            return next.run(request).await;
        }
    }

    // Collect custom strategies from all auth collections
    let auth_defs: Vec<_> = state.registry.collections.values()
        .filter(|d| d.is_auth_collection())
        .filter(|d| {
            d.auth.as_ref()
                .map(|a| !a.strategies.is_empty())
                .unwrap_or(false)
        })
        .map(|d| (d.slug.clone(), d.auth.clone().expect("guarded by filter")))
        .collect();

    if !auth_defs.is_empty() {
        // Build headers map from request (lowercase keys)
        let headers: std::collections::HashMap<String, String> = request.headers()
            .iter()
            .filter_map(|(name, value)| {
                value.to_str().ok().map(|v| (name.as_str().to_string(), v.to_string()))
            })
            .collect();

        let pool = state.pool.clone();
        let hook_runner = state.hook_runner.clone();
        let jwt_secret = state.jwt_secret.clone();

        // Try strategies in a blocking task (Lua + DB access)
        let strategy_result = tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().ok()?;
            let tx = conn.transaction().ok()?;
            let mut result = None;
            for (slug, auth_config) in &auth_defs {
                if result.is_some() { break; }
                for strategy in &auth_config.strategies {
                    match hook_runner.run_auth_strategy(
                        &strategy.authenticate,
                        slug,
                        &headers,
                        &tx,
                    ) {
                        Ok(Some(user)) => {
                            let user_email = user.fields.get("email")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let expiry = auth_config.token_expiry;
                            let claims = auth::Claims {
                                sub: user.id.clone(),
                                collection: slug.clone(),
                                email: user_email,
                                exp: (chrono::Utc::now().timestamp() as u64) + expiry,
                            };
                            result = Some((claims, jwt_secret.clone()));
                            break;
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::warn!(
                                "Auth strategy '{}' error for {}: {}",
                                strategy.name, slug, e
                            );
                            continue;
                        }
                    }
                }
            }
            // Read-only access check — commit result is irrelevant, rollback on drop is safe
            let _ = tx.commit();
            result
        }).await;

        if let Ok(Some((claims, _secret))) = strategy_result {
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale) {
                // Gate 2: Check admin.access Lua function
                if let Some(response) = check_admin_gate(&state, &auth_user).await {
                    return response;
                }
                request.extensions_mut().insert(auth_user);
            }
            request.extensions_mut().insert(claims);
            return next.run(request).await;
        }
    }

    // HTMX follows 302 redirects and swaps the response into the target,
    // which breaks standalone pages like login. Use HX-Redirect to force a
    // full page navigation instead.
    let is_htmx = request.headers()
        .get("HX-Request")
        .is_some();

    if is_htmx {
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("HX-Redirect", "/admin/login")
            .body(axum::body::Body::empty())
            .expect("static response builder")
    } else {
        Redirect::to("/admin/login").into_response()
    }
}

/// Gate 2: Check `admin.access` Lua function. Returns a 403 response if the user
/// is denied, or None if access is allowed (or no access function is configured).
// Excluded from coverage: requires HookRunner + DB pool for Lua access check.
#[cfg(not(tarpaulin_include))]
async fn check_admin_gate(state: &AdminState, auth_user: &AuthUser) -> Option<axum::response::Response> {
    let access_ref = state.config.admin.access.as_deref()?;

    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let user_doc = auth_user.user_doc.clone();
    let access_ref = access_ref.to_string();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get().ok()?;
        Some(hook_runner.check_access(Some(&access_ref), Some(&user_doc), None, None, &conn))
    }).await;

    match result {
        Ok(Some(Ok(crate::db::query::AccessResult::Denied))) => {
            Some(admin_denied_response(state))
        }
        Ok(Some(Err(e))) => {
            tracing::error!("admin.access check failed: {}", e);
            Some(admin_denied_response(state))
        }
        _ => None, // Allowed or Constrained — pass through
    }
}

/// Render the "setup required" page (no auth collection exists, require_auth is on).
fn auth_required_response(state: &AdminState) -> axum::response::Response {
    let data = serde_json::json!({});
    match state.render("errors/auth_required", &data) {
        Ok(html) => (StatusCode::SERVICE_UNAVAILABLE, axum::response::Html(html)).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "Setup required: no auth collection configured").into_response(),
    }
}

/// Render the "access denied" page (user authenticated but not authorized for admin).
fn admin_denied_response(state: &AdminState) -> axum::response::Response {
    let data = serde_json::json!({});
    match state.render("errors/admin_denied", &data) {
        Ok(html) => (StatusCode::FORBIDDEN, axum::response::Html(html)).into_response(),
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
    locale_config: &crate::config::LocaleConfig,
) -> Option<AuthUser> {
    let def = registry.get_collection(&claims.collection)?.clone();
    let locale_ctx = query::LocaleContext::from_locale_string(None, locale_config);
    let conn = pool.get().ok()?;
    let doc = query::find_by_id(&conn, &claims.collection, &def, &claims.sub, locale_ctx.as_ref()).ok()??;

    // Load ui_locale from _crap_user_settings
    let ui_locale = query::get_user_settings(&conn, &claims.sub)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("ui_locale").and_then(|l| l.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| locale_config.default_locale.clone());

    Some(AuthUser {
        claims: claims.clone(),
        user_doc: doc,
        ui_locale,
    })
}

/// Extract a named cookie value from a Cookie header string.
pub(crate) fn extract_cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    for part in header.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix(name) {
            if let Some(value) = value.strip_prefix('=') {
                return Some(value);
            }
        }
    }
    None
}

/// MCP HTTP transport handler — receives JSON-RPC 2.0 over POST /mcp.
/// Optionally validates API key from Authorization header.
// Excluded from coverage: async Axum handler requiring full server state.
#[cfg(not(tarpaulin_include))]
async fn mcp_http_handler(
    State(state): State<AdminState>,
    request: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    // API key auth — constant-time comparison to prevent timing attacks
    if !state.config.mcp.api_key.is_empty() {
        let auth_header = request.headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let expected = format!("Bearer {}", state.config.mcp.api_key);
        use subtle::ConstantTimeEq;
        let is_valid = auth_header.as_bytes().ct_eq(expected.as_bytes());
        if !bool::from(is_valid) {
            return (StatusCode::UNAUTHORIZED, "Invalid or missing API key").into_response();
        }
    }

    let body_bytes = match axum::body::to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "Request body too large").into_response(),
    };

    let rpc_request: crate::mcp::protocol::JsonRpcRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            let error_resp = crate::mcp::protocol::JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: None,
                result: None,
                error: Some(crate::mcp::protocol::JsonRpcError {
                    code: crate::mcp::protocol::PARSE_ERROR,
                    message: format!("Parse error: {}", e),
                    data: None,
                }),
            };
            return axum::Json(error_resp).into_response();
        }
    };

    let server = crate::mcp::McpServer {
        pool: state.pool.clone(),
        registry: state.registry.clone(),
        runner: state.hook_runner.clone(),
        config: state.config.clone(),
        config_dir: state.config_dir.clone(),
    };

    // Run handle_message in spawn_blocking — it does DB queries, Lua hooks, and filesystem I/O
    let response = match tokio::task::spawn_blocking(move || {
        server.handle_message(rpc_request)
    }).await {
        Ok(resp) => resp,
        Err(_) => crate::mcp::protocol::JsonRpcResponse::error(
            None,
            crate::mcp::protocol::INTERNAL_ERROR,
            "Internal error",
        ),
    };

    // Notifications must not receive a response per JSON-RPC spec
    if response.id.is_none() && response.result.is_none() && response.error.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    axum::Json(response).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_cookie_single() {
        assert_eq!(extract_cookie("crap_session=abc123", "crap_session"), Some("abc123"));
    }

    #[test]
    fn extract_cookie_multiple() {
        assert_eq!(
            extract_cookie("other=val; crap_session=token123; another=x", "crap_session"),
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
