//! Core types: collections, fields, documents, registry, auth, uploads, jobs, and validation.

pub mod auth;
pub mod collection;
pub mod document;
pub mod email;
pub mod event;
pub mod field;
pub mod field_admin_builder;
pub mod field_definition_builder;
pub mod job;
pub mod rate_limit;
pub mod registry;
pub mod richtext;
pub mod upload;
pub mod validate;

pub use collection::CollectionDefinition;
pub use document::Document;
pub use registry::{Registry, SharedRegistry};
