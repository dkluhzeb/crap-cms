use axum::{extract::State, response::Response};

use crate::admin::{
    AdminState,
    context::{AuthBasePageContext, PageMeta, PageType, page::auth::ForgotPasswordPage},
    handlers::{auth::get_auth_collections, shared::render_page},
};

/// GET /admin/forgot-password — render the forgot password form.
pub async fn forgot_password_page(State(state): State<AdminState>) -> Response {
    let auth_collections = get_auth_collections(&state);
    let show_collection_picker = auth_collections.len() > 1;

    let ctx = ForgotPasswordPage {
        base: AuthBasePageContext::for_state(
            &state,
            PageMeta::new(PageType::AuthForgot, "forgot_password_page_title"),
        ),
        success: false,
        collections: auth_collections,
        show_collection_picker,
    };

    render_page(&state, "auth/forgot_password", &ctx)
}
