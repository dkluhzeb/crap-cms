//! Static (non-CRUD) tool implementations: collection listing, describe, field types,
//! CLI reference, and config file operations.

mod config_files;
mod introspection;

pub(in crate::mcp::tools) use config_files::{
    exec_list_config_files, exec_read_config_file, exec_write_config_file,
};

// Re-export for tests
#[cfg(test)]
pub(in crate::mcp::tools) use config_files::safe_config_path;
pub(in crate::mcp::tools) use introspection::{
    exec_cli_reference, exec_describe_collection, exec_list_collections, exec_list_field_types,
};
