//! Axum router setup, auth middleware, and admin server startup.

// Auth middleware and user loading are in `auth_middleware.rs`.
use super::auth_middleware::auth_middleware;
pub(crate) use super::auth_middleware::{
    bearer_token, evaluate_admin_request, headers_to_map, load_auth_user, session_cookie_token,
};

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
    extract::{ConnectInfo, DefaultBodyLimit, MatchedPath, State},
    http::{
        Method, Request, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE,
            HeaderName, HeaderValue, SET_COOKIE,
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
use tokio::{net::TcpListener, select, spawn};
use tokio_util::sync::CancellationToken;
use tower::{Service, ServiceBuilder, timeout::TimeoutLayer};
use tower_http::{compression::CompressionLayer, trace::TraceLayer};
use tracing::{info, info_span};

use crate::{
    admin::{
        AdminState, CSP_NONCE, CspNonce, Translations, csrf,
        custom_pages::CustomPageRegistry,
        global_body_limit,
        handlers::{
            auth as auth_handlers, collections, custom_route::custom_routes_router, dashboard,
            events, globals, shared::with_error_toast, static_assets, uploads,
        },
        server_builder::AdminStartParamsBuilder,
        templates, upload_body_limit,
    },
    api::upload::upload_router,
    config::{CompressionMode, CrapConfig},
    core::{
        JwtSecret, SERVER_DRAIN_SECS, SharedPasswordProvider, drain_with_deadline,
        email::create_email_provider_with_lease, rate_limit::LoginRateLimiter,
    },
    db::{DbConnection, DbPool},
    service::AppInfra,
};

/// Parameters for starting the admin HTTP server.
///
/// All process-stable infrastructure (pool, registry, hook runner, caches,
/// transports, providers) lives in [`AppInfra`]; only the per-surface bits
/// (config, JWT secret, rate limiters, password provider) sit alongside it.
pub struct AdminStartParams {
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub jwt_secret: JwtSecret,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub mfa_limiter: Arc<LoginRateLimiter>,
    pub ip_mfa_limiter: Arc<LoginRateLimiter>,
    pub password_provider: SharedPasswordProvider,
    /// Process-stable infrastructure bundle, assembled once at boot and shared
    /// across surfaces.
    pub infra: Arc<AppInfra>,
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
/// translations, build the email provider, and resolve derived settings. The
/// process-stable infra arrives pre-assembled from boot (`params.infra`) —
/// this surface shares the same `Arc<AppInfra>` as gRPC and MCP. Kept separate
/// from the listener/serve loop so each stays focused.
fn build_admin_state(params: AdminStartParams, shutdown: CancellationToken) -> Result<AdminState> {
    let AdminStartParams {
        config,
        config_dir,
        jwt_secret,
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
        mfa_limiter,
        ip_mfa_limiter,
        password_provider,
        infra,
    } = params;
    let translations = Arc::new(Translations::load(&config_dir));
    let handlebars = templates::create_handlebars(
        &config_dir,
        config.admin.dev_mode,
        translations.clone(),
        Some(Arc::new(infra.hook_runner.clone())),
    )?;
    let custom_pages = CustomPageRegistry::from_pages(infra.hook_runner.extract_custom_pages());
    custom_pages.check_templates(|name| handlebars.get_template(name).is_some())?;
    // Pool-backed for `provider = "custom"`: admin-sent mail (password
    // reset, verification) delegates to the registered Lua handler via the
    // hook-runner VM pool.
    let email_provider =
        create_email_provider_with_lease(&config.email, infra.hook_runner.lua_lease())?;

    // Check if any auth collections exist
    let has_auth = infra
        .registry
        .collections
        .values()
        .any(|d| d.is_auth_collection());

    let max_sse_connections = config.live.max_sse_connections;
    let subscriber_send_timeout_ms = config.live.subscriber_send_timeout_ms;

    Ok(AdminState {
        mcp_sessions: Arc::default(),
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

/// Bind the listener and run the Axum server (h2c or plain) under the shared
/// bounded drain (long-lived connections may not close promptly).
#[cfg(not(tarpaulin_include))]
async fn serve_admin(
    addr: &str,
    app: Router,
    h2c_enabled: bool,
    shutdown: CancellationToken,
) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let serve_shutdown = shutdown.clone();

    let server_future: Pin<Box<dyn Future<Output = Result<()>> + Send>> = if h2c_enabled {
        info!("Admin server: h2c (HTTP/2 cleartext) enabled");

        Box::pin(serve_h2c(listener, app, serve_shutdown))
    } else {
        Box::pin(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(serve_shutdown.cancelled_owned())
            .await?;

            Ok(())
        })
    };

    drain_with_deadline(
        server_future,
        shutdown,
        Duration::from_secs(SERVER_DRAIN_SECS),
        "Admin server",
    )
    .await
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
fn method_routers(
    state: &AdminState,
) -> (
    MethodRouter<AdminState>,
    MethodRouter<AdminState>,
    MethodRouter<AdminState>,
) {
    // Create and update may carry an upload collection's file: their body
    // limit follows that collection instead of the global default.
    let upload_limit = || middleware::from_fn_with_state(state.clone(), upload_body_limit);

    let slug = get(collections::list_items)
        .merge(post(collections::create_action).route_layer(upload_limit()));
    let item = get(collections::edit_form)
        .delete(collections::delete_action)
        .merge(
            post(collections::update_action)
                .put(collections::update_action)
                .route_layer(upload_limit()),
        );
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

/// Build the protected (auth-required) sub-router and, when the deployment
/// has auth collections or `require_auth = true`, layer the auth middleware
/// on top.
#[cfg(not(tarpaulin_include))]
fn protected_with_auth(state: &AdminState) -> Router<AdminState> {
    let (slug_methods, item_methods, globals_methods) = method_routers(state);
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
        // POST carries JSON-RPC; DELETE terminates an `Mcp-Session-Id`
        // session (MCP spec's explicit session-termination request).
        Some(post(mcp_http_handler).delete(mcp_delete_session_handler))
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
            "/admin/resend-verification",
            get(auth_handlers::resend_verification_page)
                .post(auth_handlers::resend_verification_action),
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
            csrf::AUTH_CALLBACK_ROUTE,
            get(auth_handlers::auth_callback).post(auth_handlers::auth_callback),
        )
        .route(
            csrf::AUTH_CALLBACK_SCOPED_ROUTE,
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
        .layer(DefaultBodyLimit::max(global_body_limit(state)))
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

/// Readiness probe — 200 only once startup has finished AND the DB pool is
/// healthy; 503 otherwise.
///
/// Startup recovery is part of readiness, not just liveness: until the
/// scheduler has reclaimed the job rows a previous process left `running`,
/// this node's view of the queue is wrong, so an orchestrator must not route
/// traffic here or let a rolling deploy move on.
///
/// The pool checkout + probe query are blocking (checkout can park up to
/// `connection_timeout`), so they run on the blocking thread pool — a
/// stalled database must not let piling-up probe requests occupy async
/// worker threads. A join failure reports not-ready.
async fn health_readiness(State(state): State<AdminState>) -> StatusCode {
    if !state.infra.readiness.is_ready() {
        // Skip the probe entirely: the answer is already 503, and a probe
        // that parks on a pool checkout would only delay saying so.
        return readiness_status(false, false);
    }

    let pool = state.infra.pool.clone();

    let healthy = tokio::task::spawn_blocking(move || db_probe(&pool))
        .await
        .unwrap_or(false);

    readiness_status(true, healthy)
}

/// Ready only when startup has finished AND the database answers. Either one
/// missing is a 503 — a node still reclaiming job rows is as unfit to receive
/// traffic as one that can't reach its database.
fn readiness_status(startup_finished: bool, db_healthy: bool) -> StatusCode {
    if startup_finished && db_healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Check out a connection and run `SELECT 1` on it.
fn db_probe(pool: &DbPool) -> bool {
    let Ok(conn) = pool.get() else {
        return false;
    };

    conn.query_one("SELECT 1", &[]).is_ok()
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
    //
    // A handler that already chose a policy owns it: the upload serve route
    // pins `sandbox; default-src 'none'` on SVG, and replacing that with the
    // admin page policy would let a script inside an uploaded SVG run with
    // the admin origin's authority.
    if response.headers().contains_key(CONTENT_SECURITY_POLICY) {
        return response;
    }

    if let Some(csp) = state.config.admin.csp.build_header_value(Some(&nonce_str))
        && let Ok(value) = HeaderValue::from_str(&csp)
    {
        response
            .headers_mut()
            .insert(CONTENT_SECURITY_POLICY, value);
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

/// The largest urlencoded body the CSRF fallback buffers while looking for a
/// `_csrf` field.
const CSRF_FORM_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Translation key of the refusal for a missing or mismatched CSRF token.
const CSRF_FAILED_KEY: &str = "csrf_failed";

/// Translation key of the refusal for a request without the token cookie.
const CSRF_NO_COOKIE_KEY: &str = "csrf_no_cookie";

/// Translation key of the refusal for a form body too large to validate.
const CSRF_TOO_LARGE_KEY: &str = "csrf_body_too_large";

/// The answer for a mutating submit whose body is too large for the CSRF
/// fallback to read.
///
/// Not a CSRF failure: the `_csrf` field may well be in there, we simply can't
/// reach it — and answering 403 "CSRF validation failed" sent the user hunting
/// for a token problem that never existed. One helper so the declared-size and
/// discovered-while-reading cases can't drift apart.
fn csrf_body_too_large(state: &AdminState) -> Response {
    csrf_refusal(state, StatusCode::PAYLOAD_TOO_LARGE, CSRF_TOO_LARGE_KEY)
}

/// A refusal from the CSRF layer: the translation of `key` as the plain-text
/// body (a native form submit shows it) and as an `X-Crap-Toast` (an htmx
/// submit swaps nothing on an error status, so without the toast the click did
/// nothing visible).
///
/// The CSRF layer runs before the session is resolved, so the viewer's own UI
/// locale is not known yet: the message is in the admin default locale, the
/// one the login page renders in.
fn csrf_refusal(state: &AdminState, status: StatusCode, key: &str) -> Response {
    let message = state
        .translations
        .get(&state.config.locale.default_locale, key);

    with_error_toast((status, message.to_string()).into_response(), message)
}

/// Whether the request declares a body past [`CSRF_FORM_BODY_LIMIT`]. A native
/// browser form submit always declares its length, so this settles the answer
/// before a byte is read.
fn declares_oversized_body(request: &Request<Body>) -> bool {
    request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len > CSRF_FORM_BODY_LIMIT)
}

/// Validate CSRF token on a mutating request. Checks the `X-CSRF-Token` header
/// first, then falls back to the `_csrf` form field for URL-encoded bodies.
/// Returns the (possibly re-assembled) request on success, or a 403 response.
#[cfg(not(tarpaulin_include))]
async fn validate_csrf_mutation(
    state: &AdminState,
    request: Request<Body>,
    cookie_value: &str,
) -> Result<Request<Body>, Response> {
    // The header settles it without touching the body, when it is there.
    if csrf::request_token_matches(cookie_value, request.headers(), None) {
        return Ok(request);
    }

    // Otherwise the token may be the `_csrf` field of a form submit, which
    // means buffering the body and handing it back to the inner handler.
    if csrf::is_form_urlencoded(request.headers()) {
        if declares_oversized_body(&request) {
            return Err(csrf_body_too_large(state));
        }

        let (parts, body) = request.into_parts();
        let bytes = body::to_bytes(body, CSRF_FORM_BODY_LIMIT)
            .await
            .map_err(|_| csrf_body_too_large(state))?;

        if csrf::request_token_matches(cookie_value, &parts.headers, Some(&bytes)) {
            return Ok(Request::from_parts(parts, Body::from(bytes)));
        }
    }

    Err(csrf_refusal(state, StatusCode::FORBIDDEN, CSRF_FAILED_KEY))
}

/// Run the inner handler, validating the double-submit token first when the
/// method mutates.
///
/// Every exit leaves through here so the caller re-issues the `crap_csrf`
/// cookie on it — the "no token cookie" 403 included, where the cookie is
/// exactly what the browser is missing. Returning that 403 without a fresh
/// cookie made every submit fail until the user reloaded a page by hand.
#[cfg(not(tarpaulin_include))]
async fn run_csrf_checked(
    state: &AdminState,
    request: Request<Body>,
    next: Next,
    cookie_value: Option<&str>,
) -> Response {
    if !matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    ) {
        return next.run(request).await;
    }

    let Some(cookie_value) = cookie_value else {
        return csrf_refusal(state, StatusCode::FORBIDDEN, CSRF_NO_COOKIE_KEY);
    };

    match validate_csrf_mutation(state, request, cookie_value).await {
        Ok(request) => next.run(request).await,
        Err(response) => response,
    }
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
    let dev_mode = state.config.admin.dev_mode;
    let cookie_lifetime = state.config.admin.csrf_cookie_lifetime;

    // Bearer-authenticated API clients can't use double-submit cookies. CSRF
    // protects browser sessions (cookies); Bearer tokens aren't auto-attached
    // by browsers, so CSRF is irrelevant for them. Decided by the very
    // predicate that authenticates the request, so a header the evaluator
    // ignores — `Bearer` with nothing after it — can't skip the check and then
    // authenticate from the session cookie anyway.
    if bearer_token(request.headers()).is_some() {
        return next.run(request).await;
    }

    // An identity provider's form_post callback is a cross-site POST that
    // never carries the token cookie; its login-CSRF defense is the OAuth
    // `state` its hook checks.
    let matched = request.extensions().get::<MatchedPath>();

    if csrf::exempt_route(matched.map(MatchedPath::as_str)) {
        return next.run(request).await;
    }

    let cookie_header = request
        .headers()
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // An empty cookie is no cookie: it can validate nothing, so it must be
    // re-issued rather than treated as already present.
    let csrf_cookie = extract_cookie(&cookie_header, auth_handlers::CSRF_COOKIE)
        .filter(|v| !v.is_empty())
        .map(std::string::ToString::to_string);

    let mut response = run_csrf_checked(&state, request, next, csrf_cookie.as_deref()).await;

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
use super::mcp_handler::{mcp_delete_session_handler, mcp_http_handler};

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "sqlite")]
    use crate::admin::test_state::test_admin_state;

    /// A healthy database is not enough while startup recovery is still
    /// rewriting the job rows a previous process left `running` — reporting
    /// ready there lets an orchestrator send traffic to, or advance a rolling
    /// deploy past, a node whose view of the queue is still wrong.
    #[test]
    fn readiness_needs_both_startup_and_the_database() {
        assert_eq!(readiness_status(true, true), StatusCode::OK);

        assert_eq!(
            readiness_status(false, true),
            StatusCode::SERVICE_UNAVAILABLE,
            "startup recovery still running must not report ready"
        );
        assert_eq!(
            readiness_status(true, false),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            readiness_status(false, false),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// Every CSRF refusal carries an error toast, so an htmx submit that is
    /// refused does not fail silently.
    #[cfg(feature = "sqlite")]
    #[test]
    fn csrf_refusals_carry_an_error_toast() {
        let state = test_admin_state();

        for resp in [
            csrf_body_too_large(&state),
            csrf_refusal(&state, StatusCode::FORBIDDEN, CSRF_FAILED_KEY),
            csrf_refusal(&state, StatusCode::FORBIDDEN, CSRF_NO_COOKIE_KEY),
        ] {
            let toast = resp
                .headers()
                .get("X-Crap-Toast")
                .expect("toast header")
                .to_str()
                .unwrap()
                .to_string();

            assert!(toast.contains("\"type\":\"error\""), "{toast}");
            assert!(!toast.contains("csrf_"), "a raw translation key: {toast}");
        }
    }

    /// Regression: CSRF refusals were English whatever the admin locale. They
    /// speak the admin default locale.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn csrf_refusals_speak_the_admin_default_locale() {
        let mut state = test_admin_state();
        state.config.locale.default_locale = "de".to_string();

        let resp = csrf_refusal(&state, StatusCode::FORBIDDEN, CSRF_FAILED_KEY);
        let expected = state.translations.get("de", CSRF_FAILED_KEY).to_string();
        assert_ne!(
            expected,
            state.translations.get("en", CSRF_FAILED_KEY),
            "the German translation exists"
        );

        let body = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(String::from_utf8(body.to_vec()).unwrap(), expected);
    }

    /// Regression: an oversized declared body is answered by size, not by
    /// blaming the CSRF token. A body at the limit still goes to the reader.
    #[cfg(feature = "sqlite")]
    #[test]
    fn an_oversized_declared_body_is_recognised_before_reading() {
        let sized = |len: usize| {
            declares_oversized_body(
                &Request::post("/admin/collections/posts")
                    .header(CONTENT_LENGTH, len.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
        };

        assert!(sized(CSRF_FORM_BODY_LIMIT + 1));
        assert!(!sized(CSRF_FORM_BODY_LIMIT));
        assert!(!sized(0));

        // No declared length at all — the reader's own limit decides.
        assert!(!declares_oversized_body(
            &Request::post("/admin/collections/posts")
                .body(Body::empty())
                .unwrap()
        ));

        assert_eq!(
            csrf_body_too_large(&test_admin_state()).status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    /// Regression: the CSRF skip and the authenticator must agree on what a
    /// bearer request is. `Bearer` with nothing after it is not one — the
    /// evaluator ignores it and falls back to the session cookie, so skipping
    /// the CSRF check on it left a cookie-authenticated write unprotected.
    #[test]
    fn an_empty_bearer_header_is_not_a_bearer_request() {
        let bearer_of = |value: &str| {
            let request = Request::post("/admin/collections/posts")
                .header("authorization", value)
                .body(Body::empty())
                .unwrap();

            bearer_token(request.headers()).is_some()
        };

        assert!(bearer_of("Bearer abc123"));
        assert!(!bearer_of("Bearer "));
        assert!(!bearer_of("Basic abc123"));
    }

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
