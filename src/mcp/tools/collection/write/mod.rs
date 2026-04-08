//! Write operations for collection CRUD tools.

mod create;
mod delete;
mod update;

pub(in crate::mcp::tools) use create::exec_create;
pub(in crate::mcp::tools) use delete::exec_delete;
pub(in crate::mcp::tools) use update::exec_update;
