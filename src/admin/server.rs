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

use crate::config::CrapConfig;
use crate::core::SharedRegistry;
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
    registry: SharedRegistry,
    hook_runner: HookRunner,
    jwt_secret: String,
    event_bus: Option<EventBus>,
) -> Result<()> {
    let admin_locale = &config.locale.default_locale;
    let translations = std::sync::Arc::new(
        super::translations::Translations::load(&config_dir, admin_locale)
    );
    let handlebars = super::templates::create_handlebars(&config_dir, config.admin.dev_mode, translations)?;
    let email_renderer = std::sync::Arc::new(
        crate::core::email::EmailRenderer::new(&config_dir)?
    );

    // Check if any auth collections exist
    let has_auth = {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        reg.collections.values().any(|d| d.is_auth_collection())
    };

    let login_limiter = std::sync::Arc::new(
        crate::core::rate_limit::LoginRateLimiter::new(
            config.auth.max_login_attempts,
            config.auth.login_lockout_seconds,
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
    };

    let app = build_router(state, has_auth);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Build the full admin Axum router with all routes, middleware, and state.
/// Separated from `start()` so integration tests can construct the router
/// without binding to a TCP listener.
// Excluded from coverage: requires full AdminState (HookRunner with Lua VM, DB pool,
// Handlebars registry, etc). Tested indirectly through CLI integration tests.
#[cfg(not(tarpaulin_include))]
pub fn build_router(state: AdminState, has_auth: bool) -> Router {
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
        .route("/admin/globals/{slug}", globals_methods)
        .route("/admin/globals/{slug}/versions", get(globals::list_versions_page))
        .route("/admin/globals/{slug}/versions/{version_id}/restore", post(globals::restore_version))
        .route("/admin/events", get(events::sse_handler));

    // Only apply auth middleware if auth collections exist
    let protected = if has_auth {
        protected.layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
    } else {
        protected
    };

    let config_dir = &state.config_dir;

    let upload_api = crate::api::upload::upload_router(state.clone());

    let router = Router::new()
        .route("/admin/login", get(auth_handlers::login_page).post(auth_handlers::login_action))
        .route("/admin/logout", post(auth_handlers::logout_action))
        .route("/admin/forgot-password", get(auth_handlers::forgot_password_page).post(auth_handlers::forgot_password_action))
        .route("/admin/reset-password", get(auth_handlers::reset_password_page).post(auth_handlers::reset_password_action))
        .route("/admin/verify-email", get(auth_handlers::verify_email))
        .merge(protected)
        .nest("/api", upload_api)
        .nest_service("/static", static_assets::overlay_service(config_dir))
        .route("/uploads/{collection_slug}/{filename}", get(uploads::serve_upload))
        .layer(DefaultBodyLimit::max((state.config.upload.max_file_size + 1024 * 1024) as usize))
        .layer(middleware::from_fn(csrf_middleware));

    // Add CORS layer if configured (runs before CSRF in request processing)
    let router = if let Some(cors) = state.config.cors.build_layer() {
        router.layer(cors)
    } else {
        router
    };

    router.with_state(state)
}

/// CSRF middleware — double-submit cookie pattern.
/// Sets `crap_csrf` cookie on GET responses (non-HttpOnly so JS can read it).
/// Validates `X-CSRF-Token` header or `_csrf` form field on POST/PUT/DELETE.
// Excluded from coverage: async Axum middleware.
#[cfg(not(tarpaulin_include))]
async fn csrf_middleware(
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let method = request.method().clone();
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
                ensure_csrf_cookie(&mut response, csrf_cookie.as_deref());
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
                    ensure_csrf_cookie(&mut response, csrf_cookie.as_deref());
                    return response;
                }
            }
        }

        return (StatusCode::FORBIDDEN, "CSRF validation failed").into_response();
    }

    // Non-mutating method — pass through and set cookie if needed
    let mut response = next.run(request).await;
    ensure_csrf_cookie(&mut response, csrf_cookie.as_deref());
    response
}

/// Set the `crap_csrf` cookie on the response if not already present in the request.
fn ensure_csrf_cookie(
    response: &mut axum::response::Response,
    existing_cookie: Option<&str>,
) {
    if existing_cookie.is_some() {
        return;
    }
    let token = nanoid::nanoid!(32);
    let cookie = format!("crap_csrf={}; Path=/; SameSite=Strict; Max-Age=86400", token);
    if let Ok(value) = cookie.parse() {
        response.headers_mut().append(axum::http::header::SET_COOKIE, value);
    }
}

/// Auth middleware — extracts JWT from `crap_session` cookie, validates it,
/// and stores `Claims` in request extensions. If JWT is invalid/missing,
/// tries custom auth strategies before redirecting to login.
// Excluded from coverage: async Axum middleware requiring full server state (pool, registry,
// HookRunner, JWT secret) and spawned blocking tasks for Lua auth strategies.
#[cfg(not(tarpaulin_include))]
async fn auth_middleware(
    State(state): State<AdminState>,
    mut request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
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
                request.extensions_mut().insert(auth_user);
            }
            request.extensions_mut().insert(claims);
            return next.run(request).await;
        }
    }

    // Collect custom strategies from all auth collections
    let auth_defs: Vec<_> = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return Redirect::to("/admin/login").into_response(),
        };
        reg.collections.values()
            .filter(|d| d.is_auth_collection())
            .filter(|d| {
                d.auth.as_ref()
                    .map(|a| !a.strategies.is_empty())
                    .unwrap_or(false)
            })
            .map(|d| (d.slug.clone(), d.auth.clone().unwrap()))
            .collect()
    };

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
            let conn = pool.get().ok()?;
            for (slug, auth_config) in &auth_defs {
                for strategy in &auth_config.strategies {
                    match hook_runner.run_auth_strategy(
                        &strategy.authenticate,
                        slug,
                        &headers,
                        &conn,
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
                            return Some((claims, jwt_secret.clone()));
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
            None
        }).await;

        if let Ok(Some((claims, _secret))) = strategy_result {
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale) {
                request.extensions_mut().insert(auth_user);
            }
            request.extensions_mut().insert(claims);
            return next.run(request).await;
        }
    }

    Redirect::to("/admin/login").into_response()
}

/// Load the full user document for an authenticated user.
/// Returns None if the user can't be found (e.g., deleted since JWT was issued).
// Excluded from coverage: requires full DB pool + SharedRegistry with auth collection definitions.
// Tested indirectly through integration tests (admin login flow).
#[cfg(not(tarpaulin_include))]
pub(crate) fn load_auth_user(
    pool: &DbPool,
    registry: &SharedRegistry,
    claims: &auth::Claims,
    locale_config: &crate::config::LocaleConfig,
) -> Option<AuthUser> {
    let def = {
        let reg = registry.read().ok()?;
        reg.get_collection(&claims.collection)?.clone()
    };
    let locale_ctx = query::LocaleContext::from_locale_string(None, locale_config);
    let conn = pool.get().ok()?;
    let doc = query::find_by_id(&conn, &claims.collection, &def, &claims.sub, locale_ctx.as_ref()).ok()??;
    Some(AuthUser {
        claims: claims.clone(),
        user_doc: doc,
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
