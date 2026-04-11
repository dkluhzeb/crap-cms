//! `init` command — scaffold a new config directory.

mod generator;

pub(crate) use generator::LUA_API_TYPES;
pub use generator::{InitOptions, init};
