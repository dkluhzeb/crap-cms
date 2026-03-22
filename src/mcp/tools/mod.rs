//! MCP tool generation from Registry and tool execution.

mod crud_tools;
mod dispatch;
mod static_tools;

pub use dispatch::{
    ParsedTool, ToolOp, execute_tool, generate_tools, parse_tool_name, should_include,
};

// Re-export internal helpers for tests (they use `super::exec_*` etc.)
#[cfg(test)]
use crud_tools::*;
#[cfg(test)]
use static_tools::*;

#[cfg(test)]
mod tests;
