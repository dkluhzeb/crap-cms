//! MCP tool generation from Registry and tool execution.

mod collection;
mod dispatch;
mod globals;
mod schema;

pub use dispatch::{
    ParsedTool, ToolOp, execute_tool, generate_tools, parse_tool_name, should_include,
};

// Re-export internal helpers for tests
#[cfg(test)]
use collection::helpers::{doc_to_json, parse_where_filters};
#[cfg(test)]
use schema::{
    exec_cli_reference, exec_describe_collection, exec_list_collections, exec_list_config_files,
    exec_list_field_types, exec_read_config_file, exec_write_config_file, safe_config_path,
};

#[cfg(test)]
mod tests;
