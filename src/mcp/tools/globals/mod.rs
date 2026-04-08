//! Global document tool implementations.

mod get;
mod update;

pub(in crate::mcp::tools) use get::exec_read_global;
pub(in crate::mcp::tools) use update::exec_update_global;
