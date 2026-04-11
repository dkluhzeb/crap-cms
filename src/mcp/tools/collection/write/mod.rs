//! Write operations for collection CRUD tools.

mod create;
mod delete;
mod undelete;
mod unpublish;
mod update;

pub(in crate::mcp::tools) use create::exec_create;
pub(in crate::mcp::tools) use delete::exec_delete;
pub(in crate::mcp::tools) use undelete::exec_undelete;
pub(in crate::mcp::tools) use unpublish::exec_unpublish;
pub(in crate::mcp::tools) use update::exec_update;
