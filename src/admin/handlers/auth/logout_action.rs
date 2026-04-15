use axum::{
    extract::State,
    response::{IntoResponse, Redirect, Response},
};

use super::{append_cookies, clear_session_cookies, session_same_site};
use crate::admin::AdminState;

/// POST /admin/logout — clear cookies, redirect to login.
pub async fn logout_action(State(state): State<AdminState>) -> Response {
    let same_site = session_same_site(&state);
    let cookies = clear_session_cookies(state.config.admin.dev_mode, same_site);
    let mut response = Redirect::to("/admin/login").into_response();

    append_cookies(&mut response, &cookies);

    response
}
