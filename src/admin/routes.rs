//! The admin route tables: the collection / global method routers, the
//! protected (auth-required) routes, and the public pre-authentication routes.
//! [`super::server::build_router`] composes them with the middleware layers.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{MethodRouter, get, post},
};

use crate::admin::{
    AdminState, COLLECTION_ITEM_ROUTE, COLLECTION_ROUTE, auth_body_limit,
    auth_middleware::auth_middleware,
    csrf,
    handlers::{auth as auth_handlers, collections, custom_page, dashboard, events, globals},
    upload_body_limit,
};

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
        .route("/admin/p/{slug}", get(custom_page::render_custom_page))
        .route("/admin/collections", get(collections::list_collections))
        .route(COLLECTION_ROUTE, slug_methods)
        .route(
            "/admin/collections/{slug}/create",
            get(collections::create_form),
        )
        .route(COLLECTION_ITEM_ROUTE, item_methods)
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

/// Build the protected (auth-required) sub-router and, when the deployment
/// has auth collections or `require_auth = true`, layer the auth middleware
/// on top.
#[cfg(not(tarpaulin_include))]
pub(super) fn protected_with_auth(state: &AdminState) -> Router<AdminState> {
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

/// The public, pre-authentication routes (login, logout, password reset,
/// email verification, MFA, auth callbacks).
///
/// They take small forms from anyone, so their bodies are capped at
/// `[server] auth_body_limit` instead of the upload-sized global limit — the
/// route-level limit is the innermost, so it wins.
#[cfg(not(tarpaulin_include))]
pub(super) fn auth_routes(state: &AdminState) -> Router<AdminState> {
    Router::new()
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
        .route_layer(DefaultBodyLimit::max(auth_body_limit(state)))
}
