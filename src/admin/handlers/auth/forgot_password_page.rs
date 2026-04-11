use axum::{extract::State, response::Html};
use serde_json::json;
use tracing::error;

use crate::admin::{
    AdminState,
    context::{ContextBuilder, PageType},
    handlers::auth::get_auth_collections,
};

/// GET /admin/forgot-password — render the forgot password form.
pub async fn forgot_password_page(State(state): State<AdminState>) -> Html<String> {
    let auth_collections = get_auth_collections(&state);

    let data = ContextBuilder::auth(&state)
        .page(PageType::AuthForgot, "forgot_password_page_title")
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            error!("Template render error: {}", e);

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
