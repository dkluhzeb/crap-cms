use axum::{
    extract::{Query, State},
    response::Html,
};
use serde_json::json;

use super::{LoginPageQuery, all_disable_local, get_auth_collections, show_forgot_password};
use crate::admin::{
    AdminState,
    context::{ContextBuilder, PageType},
};

/// GET /admin/login — render the login page.
pub async fn login_page(
    State(state): State<AdminState>,
    query: Query<LoginPageQuery>,
) -> Html<String> {
    let auth_collections = get_auth_collections(&state);
    let all_disable_local = all_disable_local(&state);
    let show_forgot_password = show_forgot_password(&state);

    let data = ContextBuilder::auth(&state)
        .page(PageType::AuthLogin, "Login")
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .set("disable_local", json!(all_disable_local))
        .set("show_forgot_password", json!(show_forgot_password))
        .set("success", json!(query.success.as_deref()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/login", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
