use axum::{
    extract::{Query, State},
    response::Html,
};
use serde_json::json;
use tracing::error;

use crate::admin::{
    AdminState,
    context::{ContextBuilder, PageType},
    handlers::auth::{
        LoginPageQuery, all_disable_local, get_auth_collections, show_forgot_password,
    },
};

/// GET /admin/login — render the login page.
pub async fn login_page(
    State(state): State<AdminState>,
    query: Query<LoginPageQuery>,
) -> Html<String> {
    let auth_collections = get_auth_collections(&state);
    let all_disable_local = all_disable_local(&state);
    let show_forgot_password = show_forgot_password(&state);

    // Whitelist allowed success message keys to prevent arbitrary string injection
    let success = query.success.as_deref().filter(|s| {
        matches!(
            *s,
            "success_email_verified" | "success_password_reset" | "success_logout"
        )
    });

    let data = ContextBuilder::auth(&state)
        .page(PageType::AuthLogin, "login_page_title")
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .set("disable_local", json!(all_disable_local))
        .set("show_forgot_password", json!(show_forgot_password))
        .set("success", json!(success))
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/login", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            error!("Template render error: {}", e);

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
