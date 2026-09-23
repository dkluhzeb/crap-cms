//! Hook trait abstractions for read and write operations.

mod read;
mod strip;
mod write;

pub(crate) use read::ReadHooksJoinGuard;
pub use read::{LuaReadHooks, ReadHooks, RunnerReadHooks};
pub use strip::{FieldReadStrip, ReadStripArgs};
pub use write::{LuaWriteHooks, RunnerWriteHooks, SnapshotLocales, WriteHooks};
