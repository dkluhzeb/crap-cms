//! Document types for core CMS content, including the main [`Document`] type
//! and [`VersionSnapshot`] for versioning and draft support.

mod r#type;
mod version_snapshot;

pub use r#type::{Document, DocumentBuilder};
pub use version_snapshot::{VersionSnapshot, VersionSnapshotBuilder};
