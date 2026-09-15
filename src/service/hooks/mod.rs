//! Hook trait abstractions for read and write operations.

mod read;
pub(crate) mod richtext;
mod strip;
mod write;

pub(crate) use read::ReadHooksJoinGuard;
pub use read::{LuaReadHooks, ReadHooks, RunnerReadHooks};
pub use strip::{FieldReadStrip, ReadStripArgs};
pub use write::{LuaWriteHooks, RunnerWriteHooks, WriteHooks};
