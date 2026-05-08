//! Hook trait abstractions for read and write operations.

mod read_hooks;
pub(crate) mod richtext;
mod write_hooks;

pub(crate) use read_hooks::ReadHooksJoinGuard;
pub use read_hooks::{LuaReadHooks, ReadHooks, RunnerReadHooks};
pub use write_hooks::{LuaWriteHooks, RunnerWriteHooks, WriteHooks};
