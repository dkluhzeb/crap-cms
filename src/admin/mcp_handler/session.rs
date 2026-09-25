//! The `Mcp-Session-Id` header and the session a request adopts from it.

use crate::{admin::AdminState, mcp::McpServer};

/// The MCP spec's session header: returned on `initialize`, echoed by the
/// client on every later request. Lowercase (HTTP header names are
/// case-insensitive; axum normalizes to lowercase).
pub(super) const SESSION_HEADER: &str = "mcp-session-id";

/// Attach the session's client name to a per-request server, so audit lines
/// read `[client=Claude Code]` instead of `[client=(http)]` — parity with the
/// stdio transport. A missing/unknown/expired id is never an error.
pub(super) fn adopt_session(state: &AdminState, server: &McpServer, session_id: Option<&str>) {
    if let Some(id) = session_id
        && let Some(name) = state.mcp_sessions.lookup_touch(id)
    {
        let _ = server.client_name.set(name);
    }
}
