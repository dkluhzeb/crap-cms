use axum::{
    extract::{Query, State},
    response::Response,
};

use crate::admin::{
    AdminState,
    context::{AuthBasePageContext, PageMeta, PageType, page::auth::LoginPage},
    handlers::{
        auth::{LoginPageQuery, all_disable_local, get_auth_collections, show_forgot_password},
        shared::render_page,
    },
};

/// GET /admin/login — render the login page.
pub async fn login_page(State(state): State<AdminState>, query: Query<LoginPageQuery>) -> Response {
    let auth_collections = get_auth_collections(&state);
    let show_collection_picker = auth_collections.len() > 1;

    // Whitelist allowed success message keys to prevent arbitrary string injection.
    let success = query
        .success
        .as_deref()
        .filter(|s| {
            matches!(
                *s,
                "success_email_verified" | "success_password_reset" | "success_logout"
            )
        })
        .map(str::to_string);

    let ctx = LoginPage {
        base: AuthBasePageContext::for_state(
            &state,
            PageMeta::new(PageType::AuthLogin, "login_page_title"),
        ),
        error: None,
        email: None,
        collections: auth_collections,
        show_collection_picker,
        disable_local: all_disable_local(&state),
        show_forgot_password: show_forgot_password(&state),
        success,
    };

    render_page(&state, "auth/login", &ctx)
}
