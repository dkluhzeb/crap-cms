//! Global document tool implementations.

mod get;
mod update;
mod validate;

pub(in crate::mcp::tools) use get::exec_read_global;
pub(in crate::mcp::tools) use update::exec_update_global;
pub(in crate::mcp::tools) use validate::exec_validate_global;
