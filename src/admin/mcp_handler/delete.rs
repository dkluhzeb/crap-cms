//! DELETE /mcp — explicit session termination.

use std::net::SocketAddr;

use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};

use super::{auth::check_mcp_auth, session::SESSION_HEADER};
use crate::admin::AdminState;

/// DELETE /mcp — explicit session termination (MCP spec). Requires the API
/// key like every transport request; 204 when the session existed, 404
/// otherwise, 400 without the header.
#[cfg(not(tarpaulin_include))]
pub(in crate::admin) async fn mcp_delete_session_handler(
    State(state): State<AdminState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
) -> Response {
    if let Err(resp) = check_mcp_auth(&state, &request, peer_addr) {
        return *resp;
    }

    let Some(session_id) = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    if state.mcp_sessions.remove(session_id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
