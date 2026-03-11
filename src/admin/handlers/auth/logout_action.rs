use axum::{
    extract::State,
    response::{IntoResponse, Redirect},
};

use crate::admin::AdminState;
use super::clear_session_cookies;

/// GET/POST /admin/logout — clear cookies, redirect to login.
pub async fn logout_action(
    State(state): State<AdminState>,
) -> axum::response::Response {
    let cookies = clear_session_cookies(state.config.admin.dev_mode);
    let mut response = Redirect::to("/admin/login").into_response();
    for cookie in cookies {
        response.headers_mut().append(
            axum::http::header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }
    response
}
