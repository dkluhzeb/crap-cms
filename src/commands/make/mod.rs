//! `make` command — scaffold collections, globals, hooks, and jobs.

mod collection;
mod dispatch;
mod global;
mod helpers;
mod hook;
mod job;

pub(crate) use collection::make_collection_command;
pub use dispatch::run;
pub use helpers::{
    has_locales_enabled, try_load_collection_slugs, try_load_field_infos, try_load_field_names,
    try_load_registry,
};
