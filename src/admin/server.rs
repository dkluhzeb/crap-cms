//! Axum router setup, auth middleware, and admin server startup.

// Auth middleware and user loading are in `auth_middleware.rs`.
use super::auth_middleware::auth_middleware;
pub(crate) use super::auth_middleware::load_auth_user;

use std::{
    future::Future,
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};

use anyhow::Result;
use axum::{
    Router,
    body::{self, Body},
    error_handling::HandleErrorLayer,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{
        Method, Request, StatusCode,
        header::{
            AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, COOKIE, HeaderName, HeaderValue, SET_COOKIE,
        },
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{MethodRouter, get, post},
};
use hyper::service;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as AutoBuilder,
};
use nanoid::nanoid;
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, select, spawn, time::sleep};
use tokio_util::sync::CancellationToken;
use tower::{Service, ServiceBuilder, timeout::TimeoutLayer};
use tower_http::{compression::CompressionLayer, trace::TraceLayer};
use tracing::{info, info_span, warn};

use crate::{
    admin::{
        AdminState, Translations,
        handlers::{
            auth as auth_handlers, collections, dashboard, events, globals, static_assets, uploads,
        },
        server_builder::AdminStartParamsBuilder,
        templates,
    },
    api::upload::upload_router,
    config::{CompressionMode, CrapConfig},
    core::{
        JwtSecret, Registry,
        auth::{SharedPasswordProvider, SharedTokenProvider},
        email::{EmailRenderer, create_email_provider},
        event::{InProcessInvalidationBus, SharedEventTransport, SharedInvalidationTransport},
        rate_limit::LoginRateLimiter,
        upload::SharedStorage,
    },
    db::{DbConnection, DbPool},
    hooks::HookRunner,
};

/// Parameters for starting the admin HTTP server.
pub struct AdminStartParams {
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub hook_runner: HookRunner,
    pub jwt_secret: JwtSecret,
    pub event_transport: Option<SharedEventTransport>,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub storage: SharedStorage,
    pub token_provider: SharedTokenProvider,
    pub password_provider: SharedPasswordProvider,
    /// Optional shared invalidation transport — when `None`, a fresh
    /// in-process one is created.
    pub invalidation_transport: Option<SharedInvalidationTransport>,
}

impl AdminStartParams {
    /// Create a builder for `AdminStartParams`.
    pub fn builder() -> AdminStartParamsBuilder {
        AdminStartParamsBuilder::new()
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
        event_transport,
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
        storage,
        token_provider,
        password_provider,
        invalidation_transport,
    } = params;
    let translations = Arc::new(Translations::load(&config_dir));
    let handlebars =
        templates::create_handlebars(&config_dir, config.admin.dev_mode, translations.clone())?;
    let email_renderer = Arc::new(EmailRenderer::new(&config_dir)?);
    let email_provider = create_email_provider(&config.email)?;

    // Check if any auth collections exist
    let has_auth = registry
        .collections
        .values()
        .any(|d| d.is_auth_collection());

    let max_sse_connections = config.live.max_sse_connections;
    let subscriber_send_timeout_ms = config.live.subscriber_send_timeout_ms;
    let csp_header = config.admin.csp.build_header_value();
    let invalidation_transport: SharedInvalidationTransport =
        invalidation_transport.unwrap_or_else(|| Arc::new(InProcessInvalidationBus::new()));
    let state = AdminState {
        config,
        config_dir: config_dir.clone(),
        pool,
        registry,
        handlebars,
        hook_runner,
        jwt_secret,
        email_renderer,
        email_provider,
        event_transport,
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
        has_auth,
        translations,
        shutdown: shutdown.clone(),
        sse_connections: Arc::new(AtomicUsize::new(0)),
        max_sse_connections,
        csp_header,
        storage,
        token_provider,
        password_provider,
        subscriber_send_timeout_ms,
        invalidation_transport,
        populate_singleflight: Arc::new(crate::db::query::Singleflight::new()),
    };

    let h2c_enabled = state.config.server.h2c;
    let app = build_router(state);

    let listener = TcpListener::bind(addr).await?;
    let shutdown_timeout = shutdown.clone();

    let server_future: Pin<Box<dyn Future<Output = Result<()>> + Send>> = if h2c_enabled {
        info!("Admin server: h2c (HTTP/2 cleartext) enabled");

        Box::pin(serve_h2c(listener, app, shutdown))
    } else {
        Box::pin(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
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

            sleep(Duration::from_secs(10)).await;
        } => {
            warn!("Admin server: graceful shutdown timed out after 10s");
        }
    }

    Ok(())
}

/// Run the admin server with h2c (HTTP/2 cleartext) support.
/// Uses hyper-util's auto::Builder which negotiates HTTP/1.1 vs HTTP/2
/// on the same port. Reverse proxies can speak HTTP/2 to the backend
/// without TLS; browsers fall back to HTTP/1.1 gracefully.
#[cfg(not(tarpaulin_include))]
async fn serve_h2c(listener: TcpListener, app: Router, shutdown: CancellationToken) -> Result<()> {
    loop {
        select! {
            result = listener.accept() => {
                let (socket, addr) = result?;
                let tower_service = app.clone();

                spawn(async move {
                    let hyper_service = service::service_fn(move |mut req| {
                        // Insert ConnectInfo so extractors can read the client address
                        // (axum::serve does this automatically; h2c needs it manually)
                        req.extensions_mut()
                            .insert(ConnectInfo(addr));
                        tower_service.clone().call(req)
                    });

                    let io = TokioIo::new(socket);

                    AutoBuilder::new(TokioExecutor::new())
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
            "/admin/collections/{slug}/{id}/back-references",
            get(collections::back_references),
        )
        .route(
            "/admin/collections/{slug}/{id}/undelete",
            post(collections::undelete_action),
        )
        .route(
            "/admin/collections/{slug}/empty-trash",
            post(collections::empty_trash_action),
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
            "/admin/globals/{slug}/evaluate-conditions",
            post(globals::evaluate_conditions),
        )
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
        .route("/admin/logout", post(auth_handlers::logout_action))
        .route(
            "/admin/forgot-password",
            get(auth_handlers::forgot_password_page).post(auth_handlers::forgot_password_action),
        )
        .route(
            "/admin/reset-password",
            get(auth_handlers::reset_password_page).post(auth_handlers::reset_password_action),
        )
        .route("/admin/verify-email", get(auth_handlers::verify_email))
        .route(
            "/admin/mfa",
            get(auth_handlers::mfa_page).post(auth_handlers::verify_mfa_action),
        )
        .route(
            "/admin/auth/callback/{name}",
            get(auth_handlers::auth_callback).post(auth_handlers::auth_callback),
        )
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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ));

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
                let request_id = nanoid!(12);

                info_span!(
                    "http",
                    method = %req.method(),
                    path = %req.uri().path(),
                    request_id = %request_id,
                )
            })
            .on_response(
                |resp: &Response<_>, latency: Duration, _span: &tracing::Span| {
                    info!(
                        status = resp.status().as_u16(),
                        latency_ms = latency.as_millis(),
                        "response"
                    );
                },
            ),
    );

    // Add request timeout if configured. Uses HandleErrorLayer to convert
    // tower::timeout errors into 408 Request Timeout responses.
    let router = if let Some(timeout_secs) = state.config.server.request_timeout {
        router.layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|_| async {
                    StatusCode::REQUEST_TIMEOUT
                }))
                .layer(TimeoutLayer::new(Duration::from_secs(timeout_secs))),
        )
    } else {
        router
    };

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
async fn security_headers(
    State(state): State<AdminState>,
    request: Request<Body>,
    next: Next,
) -> Response {
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

    if let Some(ref csp) = state.csp_header
        && let Ok(value) = HeaderValue::from_str(csp)
    {
        headers.insert(HeaderName::from_static("content-security-policy"), value);
    }

    // HSTS: instruct browsers to always use HTTPS (skip in dev mode)
    if !state.config.admin.dev_mode {
        headers.insert(
            HeaderName::from_static("strict-transport-security"),
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }

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

    if let Some(ct) = response.headers().get(CONTENT_TYPE)
        && ct.to_str().unwrap_or("").starts_with("text/html")
    {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
        .get(CONTENT_TYPE)
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
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer "));

    if has_bearer {
        return next.run(request).await;
    }

    let cookie_header = request
        .headers()
        .get(COOKIE)
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

    let token = nanoid!(32);
    let secure = if dev_mode { "" } else { "; Secure" };
    let cookie = format!(
        "crap_csrf={}; Path=/; SameSite=Strict; Max-Age=86400{}",
        token, secure
    );

    if let Ok(value) = cookie.parse() {
        response.headers_mut().append(SET_COOKIE, value);
    }
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

// MCP HTTP handler is in `mcp_handler.rs`.
use super::mcp_handler::mcp_http_handler;

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
