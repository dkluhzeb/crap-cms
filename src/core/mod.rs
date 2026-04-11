//! Core types: collections, fields, documents, registry, auth, uploads, jobs, and validation.

pub mod auth;
pub mod cache;
pub mod collection;
pub mod document;
pub mod document_id;
pub mod email;
pub mod event;
pub mod field;
pub mod job;
pub mod rate_limit;
pub mod registry;
pub mod richtext;
pub mod slug;
pub mod timezone;
pub mod upload;
pub mod validate;

pub use auth::{AuthUser, Claims, HashedPassword, JwtSecret, ResetTokenError};
pub use collection::CollectionDefinition;
pub use document::Document;
pub use document_id::DocumentId;
pub use field::{
    BlockDefinition, FieldAdmin, FieldDefinition, FieldTab, FieldType, LocalizedString,
    RelationshipConfig, SelectOption,
};
pub use registry::{Registry, SharedRegistry};
pub use slug::Slug;
