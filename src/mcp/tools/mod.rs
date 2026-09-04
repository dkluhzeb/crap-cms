//! MCP tool generation from Registry and tool execution.

mod collection;
mod dispatch;
mod exec_ctx;
mod globals;
pub(in crate::mcp::tools) mod jobs;
mod schema;

pub(in crate::mcp) use exec_ctx::ToolExecCtx;

pub(in crate::mcp) use dispatch::{execute_tool, generate_tools, slug_exposed};

#[cfg(test)]
mod test_helpers;
