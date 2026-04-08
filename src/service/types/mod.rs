//! Shared type definitions for the service layer.

mod after_change_input;
mod persist_options;
mod write_input;
mod write_result;

pub(crate) use after_change_input::AfterChangeInput;
pub use persist_options::{PersistOptions, PersistOptionsBuilder};
pub use write_input::{WriteInput, WriteInputBuilder};
pub use write_result::WriteResult;
