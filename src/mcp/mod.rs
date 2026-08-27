//! MCP (Model Context Protocol) server for Crap CMS.
//!
//! Provides auto-generated tool definitions from the Lua-defined Registry,
//! schema introspection resources, and optional config generation tools.
//! Supports stdio and HTTP transports.
//!
//! # External surface
//! - [`McpServer`] — the server struct, instantiated by callers.
//! - [`run_stdio`] — entry point for the stdio transport.
//! - JSON-RPC types (`JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`,
//!   and the `INTERNAL_ERROR` / `INVALID_REQUEST` / `PARSE_ERROR` codes)
//!   re-exported for the HTTP transport in `admin::mcp_handler`.
//!
//! Internal modules (`schema`, `resources`, `tools`) are `pub(crate)`
//! and not part of the external API.

pub(crate) mod access;
pub(crate) mod infra;
pub(crate) mod protocol;
pub(crate) mod resources;
pub(crate) mod schema;
pub(crate) mod server;
pub(crate) mod stdio;
pub(crate) mod tools;

pub use protocol::{
    INTERNAL_ERROR, INVALID_REQUEST, JsonRpcError, JsonRpcRequest, JsonRpcResponse, PARSE_ERROR,
};
pub use server::McpServer;
pub use stdio::run_stdio;
