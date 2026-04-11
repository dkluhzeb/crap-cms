//! Hook trait abstractions for read and write operations.

mod read_hooks;
pub(crate) mod richtext;
mod write_hooks;

pub use read_hooks::{LuaReadHooks, LuaReadHooksBuilder, ReadHooks, RunnerReadHooks};
pub use write_hooks::{LuaWriteHooks, LuaWriteHooksBuilder, RunnerWriteHooks, WriteHooks};
