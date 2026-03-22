//! Shared helper functions for auth handlers.

use axum::response::{Html, IntoResponse, Response};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
    },
    core::email,
};

pub(in crate::admin::handlers) fn login_error(
    state: &AdminState,
    error: &str,
    email: &str,
) -> Response {
    let auth_collections = get_auth_collections(state);
    let all_disable_local = all_disable_local(state);
    let show_forgot_password = show_forgot_password(state);

    let data = ContextBuilder::auth(state)
        .page(PageType::AuthLogin, "Login")
        .set("error", json!(error))
        .set("email", json!(email))
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .set("disable_local", json!(all_disable_local))
        .set("show_forgot_password", json!(show_forgot_password))
        .build();

    match state.render("auth/login", &data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
        .into_response(),
    }
}

/// Check if all auth collections have disable_local = true.
pub(in crate::admin::handlers) fn all_disable_local(state: &AdminState) -> bool {
    let auth_collections: Vec<_> = state
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .collect();

    if auth_collections.is_empty() {
        return false;
    }

    auth_collections
        .iter()
        .all(|def| def.auth.as_ref().map(|a| a.disable_local).unwrap_or(false))
}

/// Check if "forgot password?" link should show on login page.
pub(in crate::admin::handlers) fn show_forgot_password(state: &AdminState) -> bool {
    if !email::is_configured(&state.config.email) {
        return false;
    }

    state
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .any(|def| def.auth.as_ref().is_some_and(|a| a.forgot_password))
}

pub(in crate::admin::handlers) fn get_auth_collections(state: &AdminState) -> Vec<Value> {
    let mut collections: Vec<_> = state
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .map(|def| {
            json!({
                "slug": def.slug,
                "display_name": def.display_name(),
            })
        })
        .collect();

    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    collections
}

pub(in crate::admin::handlers) fn render_forgot_success(
    state: &AdminState,
    auth_collections: &[Value],
) -> Html<String> {
    let data = ContextBuilder::auth(state)
        .page(PageType::AuthForgot, "Forgot Password")
        .set("success", json!(true))
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .build();

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
