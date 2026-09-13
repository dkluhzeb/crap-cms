//! The "this tool is not part of the surface" error.
//!
//! One error covers three cases that must stay indistinguishable to a
//! caller: the tool name does not exist, the collection behind it is
//! filtered out by the `[mcp]` include/exclude lists, and the collection
//! is hidden by its `access.mcp` rule. Reporting them differently would
//! let an MCP client enumerate collections it is not allowed to see.
//!
//! It is a typed error rather than a plain message so the JSON-RPC layer
//! can answer with an `Invalid params` protocol error, as the MCP
//! specification requires for an unknown tool, instead of a tool result.

use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// A tool name this server does not expose to the caller.
#[derive(Debug)]
pub(in crate::mcp) struct UnknownTool(String);

impl UnknownTool {
    pub(in crate::mcp) fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

impl Display for UnknownTool {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "Unknown tool: {}", self.0)
    }
}

impl Error for UnknownTool {}

#[cfg(test)]
mod tests {
    use super::UnknownTool;

    #[test]
    fn the_message_names_the_tool_and_nothing_else() {
        assert_eq!(
            UnknownTool::new("find_secrets").to_string(),
            "Unknown tool: find_secrets"
        );
    }
}
