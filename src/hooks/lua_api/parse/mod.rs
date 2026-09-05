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
pub(crate) use fields::FIELD_HOOK_KEYS;
pub use global::parse_global_definition;
pub use job::{JobDefinitionConfig, parse_job_definition};
pub(crate) use shared::{ACCESS_KEYS, COLLECTION_HOOK_KEYS, GLOBAL_ACCESS_KEYS};

pub(crate) use helpers::{deny_unknown_keys, get_bool, get_string_strict};
