//! Hook trait abstractions for read and write operations.

mod read_hooks;
pub(crate) mod richtext;
mod write_hooks;

pub use read_hooks::{LuaReadHooks, ReadHooks, RunnerReadHooks};
pub(crate) use richtext::apply_richtext_before_validate;
pub use write_hooks::{LuaWriteHooks, RunnerWriteHooks, WriteHooks};
