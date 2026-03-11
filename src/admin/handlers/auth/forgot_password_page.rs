use axum::{extract::State, response::Html};
use serde_json::json;

use super::get_auth_collections;
use crate::admin::{
    AdminState,
    context::{ContextBuilder, PageType},
};

/// GET /admin/forgot-password — render the forgot password form.
pub async fn forgot_password_page(State(state): State<AdminState>) -> Html<String> {
    let auth_collections = get_auth_collections(&state);

    let data = ContextBuilder::auth(&state)
        .page(PageType::AuthForgot, "Forgot Password")
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
