//! Tonic gRPC service implementing all ContentAPI RPCs.

mod auth;
mod collection;
mod content_service;
mod content_service_deps;
pub(crate) mod convert;
mod globals;
mod jobs;
mod schema;
mod subscribe;

pub use content_service::ContentService;
pub use content_service_deps::{ContentServiceDeps, ContentServiceDepsBuilder};
