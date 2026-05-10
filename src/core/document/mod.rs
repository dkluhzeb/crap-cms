//! Document types for core CMS content, including the main [`Document`] type
//! and [`VersionSnapshot`] for versioning and draft support.

mod kind;
mod version_snapshot;

pub use kind::{Document, DocumentBuilder};
pub use version_snapshot::{VersionSnapshot, VersionSnapshotBuilder};
