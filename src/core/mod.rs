//! Core types: collections, fields, documents, registry, auth, uploads, jobs, and validation.

pub mod auth;
pub mod email;
pub mod event;
pub mod field;
pub mod collection;
pub mod document;
pub mod job;
pub mod registry;
pub mod upload;
pub mod validate;

pub use collection::CollectionDefinition;
pub use document::Document;
pub use registry::{Registry, SharedRegistry};
