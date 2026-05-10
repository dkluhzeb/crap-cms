//! Parsing functions for collection/global/job Lua definitions into Rust types.

mod admin;
mod auth;
mod blocks;
mod collection;
pub(super) mod fields;
mod global;
mod helpers;
mod job;
mod relationship;
mod shared;
mod upload;

pub use collection::parse_collection_definition;
pub use global::parse_global_definition;
pub use job::parse_job_definition;
