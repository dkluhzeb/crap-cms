//! Hook trait abstractions for read and write operations.

mod read;
mod read_probe;
mod strip;
mod write;

pub(crate) use read::ReadHooksJoinGuard;
pub use read::{LuaReadHooks, ReadHooks, RunnerReadHooks};
pub(crate) use read_probe::{RowSchema, TemplateRows, is_template_row, mark_absent};
pub use strip::{FieldReadStrip, ReadStripArgs};
pub(crate) use write::update_strip_needs_stored;
pub use write::{
    LuaWriteHooks, RunnerWriteHooks, SnapshotLocales, SnapshotReadKeep, StoredByLocale,
    UpdateStored, WriteHooks,
};
