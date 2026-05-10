//! `crap-cms templates` — manage and inspect the user's customization
//! layer (the files in `<config_dir>/{templates,static}/` that override
//! the compiled-in defaults).
//!
//! - [`list`] / [`extract`] — bootstrap helpers for getting starter files
//!   from the embedded defaults.
//! - [`status`] walks the customized files, parses the `crap-cms:source
//!   <version>` header from each, and reports the relationship to the
//!   running crap-cms version. [`customization_counts`] returns the
//!   same data as a struct for the main `crap-cms status` command.
//! - [`layout`] reports old-layout files and recommends `git mv`
//!   commands (read-only).
//! - [`diff`] takes one customized file and shows a unified-style diff
//!   between the user's copy and the embedded default.
//!
//! One file per `crap-cms templates <action>` subcommand, plus
//! `shared.rs` for helpers reused by `status` and `diff`
//! (`split_kind`, `lookup_embedded`, the `CRATE_VERSION` constant).

mod diff;
mod extract;
mod layout;
mod list;
mod shared;
mod status;

pub use diff::diff;
pub use extract::extract;
pub use layout::layout;
pub use list::list;
pub use status::{CustomizationCounts, customization_counts, status};
