//! Static (non-CRUD) tool implementations: collection listing, describe, field types,
//! CLI reference, and config file operations.

mod cli_details;
mod cli_reference;
mod config_files;
mod describe_collection;
mod field_types;
mod list_collections;

pub(in crate::mcp::tools) use cli_reference::exec_cli_reference;
pub(in crate::mcp::tools) use config_files::{
    exec_list_config_files, exec_read_config_file, exec_write_config_file,
};
pub(in crate::mcp::tools) use describe_collection::exec_describe_collection;
pub(in crate::mcp::tools) use field_types::exec_list_field_types;
pub(in crate::mcp::tools) use list_collections::exec_list_collections;
