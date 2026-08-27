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
        AdminState, CSP_NONCE, CspNonce, Translations,
        handlers::{
            auth as auth_handlers, collections, custom_route::custom_routes_router, dashboard,
            events, globals, static_assets, uploads,
        },
        server_builder::AdminStartParamsBuilder,
        templates,
    },
    api::upload::upload_router,
    config::{CompressionMode, CrapConfig},
    core::{
        CollectionDefinition, JwtSecret, Registry, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, SharedPasswordProvider, SharedStorage, SharedTokenProvider,
        cache::NoneCache,
        email::{EmailRenderer, create_email_provider_with_lease},
        event::InProcessInvalidationBus,
        rate_limit::LoginRateLimiter,
    },
    db::{DbConnection, DbPool},
    hooks::HookRunner,
    service::{AppInfra, EmailContext},
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
    pub mfa_limiter: Arc<LoginRateLimiter>,
    pub ip_mfa_limiter: Arc<LoginRateLimiter>,
    pub storage: SharedStorage,
    pub token_provider: SharedTokenProvider,
    pub password_provider: SharedPasswordProvider,
    /// Optional shared invalidation transport — when `None`, a fresh
    /// in-process one is created.
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    /// Shared cross-request cache for populated relationship documents.
    /// Passed to service-layer write operations for cache invalidation.
    pub cache: Option<SharedCache>,
}

impl AdminStartParams {
    /// Create a builder for `AdminStartParams`.
    #[must_use]
    pub fn builder() -> AdminStartParamsBuilder {
        AdminStartParamsBuilder::new()
    }
}

/// Start the admin HTTP server (Axum) with all routes, middleware, and static file serving.
///
/// # Errors
///
/// Returns an error if the TCP listener can't bind, the router fails to
/// build, or the server hits an unrecoverable runtime error.
// Excluded from coverage: async server startup orchestration (binds TCP listener, runs Axum server).
#[cfg(not(tarpaulin_include))]
pub async fn start(
    addr: &str,
    params: AdminStartParams,
    shutdown: CancellationToken,
) -> Result<()> {
    let state = build_admin_state(params, shutdown.clone())?;

    let h2c_enabled = state.config.server.h2c;
    let app = build_router(state);

    serve_admin(addr, app, h2c_enabled, shutdown).await
}

/// Assemble the [`AdminState`] from the start-params: load templates and
/// translations, build the email renderer/provider, and resolve derived
/// settings. Kept separate from the listener/serve loop so each stays focused.
fn build_admin_state(params: AdminStartParams, shutdown: CancellationToken) -> Result<AdminState> {
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
        mfa_limiter,
        ip_mfa_limiter,
        storage,
        token_provider,
        password_provider,
        invalidation_transport,
        cache,
    } = params;
    let translations = Arc::new(Translations::load(&config_dir));
    let handlebars = templates::create_handlebars(
        &config_dir,
        config.admin.dev_mode,
        translations.clone(),
        Some(Arc::new(hook_runner.clone())),
    )?;
    let custom_pages = crate::admin::custom_pages::CustomPageRegistry::from_pages(
        hook_runner.extract_custom_pages(),
    );
    let email_renderer = Arc::new(EmailRenderer::new(&config_dir)?);
    // Pool-backed for `provider = "custom"`: admin-sent mail (password
    // reset, verification) delegates to the registered Lua handler via the
    // hook-runner VM pool.
    let email_provider = create_email_provider_with_lease(&config.email, hook_runner.lua_lease())?;

    // Check if any auth collections exist
    let has_auth = registry
        .collections
        .values()
        .any(CollectionDefinition::is_auth_collection);

    let max_sse_connections = config.live.max_sse_connections;
    let subscriber_send_timeout_ms = config.live.subscriber_send_timeout_ms;
    let invalidation_transport: SharedInvalidationTransport =
        invalidation_transport.unwrap_or_else(|| Arc::new(InProcessInvalidationBus::new()));

    // Assemble the process-stable infra bundle once from the admin dependencies.
    // Handlers read individual deps from it (`state.infra.pool`, …) and thread it
    // into write contexts via `.infra()`; the individual fields are gone.
    let infra = Arc::new(
        AppInfra::builder()
            .pool(pool)
            .registry(Arc::clone(&registry))
            .hook_runner(hook_runner)
            .cache(cache.unwrap_or_else(|| Arc::new(NoneCache)))
            .storage(storage)
            .event_transport(event_transport)
            .invalidation_transport(invalidation_transport)
            .token_provider(token_provider)
            .email(EmailContext {
                email_config: config.email.clone(),
                email_renderer,
                server_config: config.server.clone(),
                email_max_attempts: config.jobs.system_email_max_attempts(),
            })
            .locale_config(config.locale.clone())
            .password_policy(config.auth.password_policy.clone())
            .populate_singleflight(Arc::new(crate::db::query::Singleflight::new()))
            .build(),
    );

    Ok(AdminState {
        infra,
        config,
        config_dir: config_dir.clone(),
        handlebars,
        jwt_secret,
        email_provider,
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
        mfa_limiter,
        ip_mfa_limiter,
        has_auth,
        translations,
        shutdown,
        sse_connections: Arc::new(AtomicUsize::new(0)),
        max_sse_connections,
        password_provider,
        subscriber_send_timeout_ms,
        custom_pages,
    })
}

/// Bind the listener and run the Axum server (h2c or plain) with a graceful
/// shutdown and a hard 10s drain deadline.
#[cfg(not(tarpaulin_include))]
async fn serve_admin(
    addr: &str,
    app: Router,
    h2c_enabled: bool,
    shutdown: CancellationToken,
) -> Result<()> {
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
        () = async {
            shutdown_timeout.cancelled().await;

            sleep(Duration::from_secs(10)).await;
        } => {
            warn!("Admin server: graceful shutdown timed out after 10s");
        }
    }

    Ok(())
}

/// Run the admin server with h2c (HTTP/2 cleartext) support.
/// Uses hyper-util's `auto::Builder` which negotiates HTTP/1.1 vs HTTP/2
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
            () = shutdown.cancelled() => break,
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
        .route(
            "/admin/p/{slug}",
            get(crate::admin::handlers::custom_page::render_custom_page),
        )
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
    let protected = protected_with_auth(&state);
    let upload_api = upload_router(state.clone());

    // Built-in routes carry the global double-submit CSRF / cache / security
    // layers.
    let base = assemble_base_router(&state, protected, upload_api);
    let base = with_request_layers(base, &state);

    // Custom routes are API-style: they bypass the global CSRF middleware
    // (per-route `csrf = true` is enforced inside the dispatcher) and carry their
    // own per-route body-size limit. Merged AFTER the CSRF layer so it doesn't
    // wrap them.
    let router = base.merge(custom_routes_router(&state));

    // Static protective headers (frame-options, nosniff, referrer, permissions,
    // HSTS) apply to the FULL router — including custom routes, which would
    // otherwise ship with none. The nonce-bound admin CSP stays base-only.
    let router = router.layer(middleware::from_fn_with_state(
        state.clone(),
        static_security_headers,
    ));

    let router = with_cors_layer(router, &state);
    let router = with_compression_layer(router, &state);
    let router = with_tracing_layer(router);
    let router = with_timeout_layer(router, &state);

    router.with_state(state)
}

/// The maximum admin request body size in bytes (upload cap + 1 MiB headroom),
/// clamped to 50 MiB if the addition would overflow `usize`.
fn request_body_limit(state: &AdminState) -> usize {
    usize::try_from(state.config.upload.max_file_size + 1024 * 1024).unwrap_or(50 * 1024 * 1024)
}

/// Build the protected (auth-required) sub-router and, when the deployment
/// has auth collections or `require_auth = true`, layer the auth middleware
/// on top.
#[cfg(not(tarpaulin_include))]
fn protected_with_auth(state: &AdminState) -> Router<AdminState> {
    let (slug_methods, item_methods, globals_methods) = method_routers();
    let protected = protected_routes(slug_methods, item_methods, globals_methods);

    let needs_auth_layer = state.has_auth || state.config.admin.require_auth;
    if needs_auth_layer {
        protected.layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
    } else {
        protected
    }
}

/// Compose the public auth routes, the protected sub-router, the optional
/// MCP HTTP endpoint, the upload API, and the static-asset / upload-serving
/// routes into a single base router (no middleware layers yet).
#[cfg(not(tarpaulin_include))]
fn assemble_base_router(
    state: &AdminState,
    protected: Router<AdminState>,
    upload_api: Router<AdminState>,
) -> Router<AdminState> {
    let mcp_route = if state.config.mcp.enabled && state.config.mcp.http {
        Some(post(mcp_http_handler))
    } else {
        None
    };
    let mcp_router = mcp_route.map_or_else(Router::new, |mcp| Router::new().route("/mcp", mcp));

    Router::new()
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
        .route(
            "/admin/auth/callback/{collection}/{name}",
            get(auth_handlers::auth_callback_scoped).post(auth_handlers::auth_callback_scoped),
        )
        .merge(protected)
        .merge(mcp_router)
        .nest("/api", upload_api)
        .nest_service("/static", static_assets::overlay_service(&state.config_dir))
        .route(
            "/uploads/{collection_slug}/{filename}",
            get(uploads::serve_upload),
        )
}

/// Apply the always-on request layers: body-size limit, CSRF, HTML cache
/// control, and security headers (X-Frame-Options / CSP / etc).
#[cfg(not(tarpaulin_include))]
fn with_request_layers(router: Router<AdminState>, state: &AdminState) -> Router<AdminState> {
    router
        .layer(DefaultBodyLimit::max(request_body_limit(state)))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf_middleware,
        ))
        .layer(middleware::from_fn(html_cache_control))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ))
}

/// Apply the configured CORS layer (no-op when CORS is disabled).
#[cfg(not(tarpaulin_include))]
fn with_cors_layer(router: Router<AdminState>, state: &AdminState) -> Router<AdminState> {
    if let Some(cors) = state.config.cors.build_layer() {
        router.layer(cors)
    } else {
        router
    }
}

/// Apply the configured response compression (gzip / brotli / both / off).
#[cfg(not(tarpaulin_include))]
fn with_compression_layer(router: Router<AdminState>, state: &AdminState) -> Router<AdminState> {
    match state.config.server.compression {
        CompressionMode::Off => router,
        CompressionMode::Gzip => {
            router.layer(CompressionLayer::new().no_br().no_deflate().no_zstd())
        }
        CompressionMode::Br => {
            router.layer(CompressionLayer::new().no_gzip().no_deflate().no_zstd())
        }
        CompressionMode::All => router.layer(CompressionLayer::new()),
    }
}

/// Apply per-request tracing: spans with method, path, status, latency, and
/// a 12-char request id propagated through the response.
#[cfg(not(tarpaulin_include))]
fn with_tracing_layer(router: Router<AdminState>) -> Router<AdminState> {
    router.layer(
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
    )
}

/// Apply the configured request-timeout layer, mapping tower timeout errors
/// to a 408 Request Timeout response.
#[cfg(not(tarpaulin_include))]
fn with_timeout_layer(router: Router<AdminState>, state: &AdminState) -> Router<AdminState> {
    let Some(timeout_secs) = state.config.server.request_timeout else {
        return router;
    };
    router.layer(
        ServiceBuilder::new()
            .layer(HandleErrorLayer::new(|_| async {
                StatusCode::REQUEST_TIMEOUT
            }))
            .layer(TimeoutLayer::new(Duration::from_secs(timeout_secs))),
    )
}

/// Liveness probe — always returns 200 OK.
async fn health_liveness() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe — returns 200 if DB pool is healthy, 503 otherwise.
async fn health_readiness(State(state): State<AdminState>) -> StatusCode {
    match state.infra.pool.get() {
        Ok(conn) => match conn.query_one("SELECT 1", &[]) {
            Ok(_) => StatusCode::OK,
            Err(_) => StatusCode::SERVICE_UNAVAILABLE,
        },
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Security headers middleware — sets protective headers on every response.
///
/// Generates a fresh Content-Security-Policy nonce for each request,
/// scopes it into a task-local for the duration of the inner service so
/// templates can emit `<script nonce="...">`, and then stamps both the
/// nonce-bearing CSP header and the usual static protection headers onto
/// the response.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn security_headers(
    State(state): State<AdminState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let nonce = CspNonce::generate();
    let nonce_str = nonce.as_str().to_string();

    // Scope the nonce into a task-local so `CrapMeta::from_state` can pick
    // it up when assembling the template context for this request.
    let mut response = CSP_NONCE.scope(nonce, next.run(request)).await;

    // The nonce-bearing CSP is admin-only: it names the per-request nonce the
    // admin templates emit. Custom routes render their own bodies (no nonce),
    // so they must NOT inherit this CSP — they get the static protective
    // headers via `static_security_headers` on the full router instead.
    if let Some(csp) = state.config.admin.csp.build_header_value(Some(&nonce_str))
        && let Ok(value) = HeaderValue::from_str(&csp)
    {
        response
            .headers_mut()
            .insert(HeaderName::from_static("content-security-policy"), value);
    }

    response
}

/// Static protective headers applied to **every** response — built-in admin
/// routes and merged custom routes alike. Unlike the nonce-bound CSP (which is
/// admin-template-specific and lives in [`security_headers`]), these are
/// content-independent, so a custom Lua route also gets clickjacking,
/// MIME-sniffing, referrer, permissions, and HSTS protection.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn static_security_headers(
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
        .map(std::string::ToString::to_string);

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
    let cookie_lifetime = state.config.admin.csrf_cookie_lifetime;

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

    let csrf_cookie = extract_cookie(&cookie_header, auth_handlers::CSRF_COOKIE)
        .map(std::string::ToString::to_string);

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

                ensure_csrf_cookie(
                    &mut response,
                    csrf_cookie.as_deref(),
                    dev_mode,
                    cookie_lifetime,
                );

                return response;
            }
            Err(response) => return response,
        }
    }

    // Non-mutating method — pass through and set cookie if needed
    let mut response = next.run(request).await;

    ensure_csrf_cookie(
        &mut response,
        csrf_cookie.as_deref(),
        dev_mode,
        cookie_lifetime,
    );

    response
}

/// Set the `crap_csrf` cookie on the response if not already present in the request.
/// Adds `Secure` flag in production mode (same as session cookies).
/// `lifetime` is the `Max-Age` in seconds, sourced from `admin.csrf_cookie_lifetime`.
fn ensure_csrf_cookie(
    response: &mut Response,
    existing_cookie: Option<&str>,
    dev_mode: bool,
    lifetime: u64,
) {
    if existing_cookie.is_some() {
        return;
    }

    let token = nanoid!(32);
    let secure = if dev_mode { "" } else { "; Secure" };
    let cookie = format!("crap_csrf={token}; Path=/; SameSite=Strict; Max-Age={lifetime}{secure}");

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
