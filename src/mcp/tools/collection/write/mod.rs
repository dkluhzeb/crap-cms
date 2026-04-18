//! Write operations for collection CRUD tools.

mod create;
mod create_many;
mod delete;
mod delete_many;
mod undelete;
mod unpublish;
mod update;
mod update_many;

pub(in crate::mcp::tools) use create::exec_create;
pub(in crate::mcp::tools) use create_many::exec_create_many;
pub(in crate::mcp::tools) use delete::exec_delete;
pub(in crate::mcp::tools) use delete_many::exec_delete_many;
pub(in crate::mcp::tools) use undelete::exec_undelete;
pub(in crate::mcp::tools) use unpublish::exec_unpublish;
pub(in crate::mcp::tools) use update::exec_update;
pub(in crate::mcp::tools) use update_many::exec_update_many;
