//! MCP HTTP transport handlers — JSON-RPC 2.0 over POST /mcp, with
//! `Mcp-Session-Id` session tracking (per-session client identity for audit
//! logs) and DELETE-based session termination.

mod auth;
mod delete;
mod post;
mod session;

pub(super) use delete::mcp_delete_session_handler;
pub(super) use post::mcp_http_handler;
