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
use std::path::PathBuf;

use crate::config::CrapConfig;
use crate::core::SharedRegistry;
use crate::core::auth::{self, AuthUser};
use crate::db::DbPool;
use crate::db::query;
use crate::hooks::lifecycle::HookRunner;
use super::AdminState;
use super::handlers::{auth as auth_handlers, dashboard, collections, globals, static_assets, uploads};

pub async fn start(
    addr: &str,
    config: CrapConfig,
    config_dir: PathBuf,
    pool: DbPool,
    registry: SharedRegistry,
    hook_runner: HookRunner,
    jwt_secret: String,
) -> Result<()> {
    let handlebars = super::templates::create_handlebars(&config_dir, config.admin.dev_mode)?;

    // Check if any auth collections exist
    let has_auth = {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        reg.collections.values().any(|d| d.is_auth_collection())
    };

    let state = AdminState {
        config,
        config_dir: config_dir.clone(),
        pool,
        registry,
        handlebars,
        hook_runner,
        jwt_secret,
    };

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
        .route("/admin/globals/{slug}", globals_methods);

    // Only apply auth middleware if auth collections exist
    let protected = if has_auth {
        protected.layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
    } else {
        protected
    };

    let app = Router::new()
        .route("/admin/login", get(auth_handlers::login_page).post(auth_handlers::login_action))
        .route("/admin/logout", post(auth_handlers::logout_action))
        .merge(protected)
        .nest_service("/static", static_assets::overlay_service(&config_dir))
        .route("/uploads/{collection_slug}/{filename}", get(uploads::serve_upload))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Auth middleware — extracts JWT from `crap_session` cookie, validates it,
/// and stores `Claims` in request extensions. If JWT is invalid/missing,
/// tries custom auth strategies before redirecting to login.
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
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims) {
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
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims) {
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
pub(crate) fn load_auth_user(
    pool: &DbPool,
    registry: &SharedRegistry,
    claims: &auth::Claims,
) -> Option<AuthUser> {
    let def = {
        let reg = registry.read().ok()?;
        reg.get_collection(&claims.collection)?.clone()
    };
    let conn = pool.get().ok()?;
    let doc = query::find_by_id(&conn, &claims.collection, &def, &claims.sub).ok()??;
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
