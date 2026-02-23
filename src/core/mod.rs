//! Core types: collections, fields, documents, registry, auth, uploads, and validation.

pub mod auth;
pub mod email;
pub mod field;
pub mod collection;
pub mod document;
pub mod registry;
pub mod upload;
pub mod validate;

pub use collection::CollectionDefinition;
pub use document::Document;
pub use registry::{Registry, SharedRegistry};
