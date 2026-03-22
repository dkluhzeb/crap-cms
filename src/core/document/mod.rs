//! Document types for core CMS content, including the main [`Document`] type
//! and [`VersionSnapshot`] for versioning and draft support.

mod document_builder;
mod r#type;
pub mod version_snapshot;
pub mod version_snapshot_builder;

pub use document_builder::DocumentBuilder;
pub use r#type::Document;
pub use version_snapshot::VersionSnapshot;
pub use version_snapshot_builder::VersionSnapshotBuilder;
