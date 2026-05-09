//! Registers the `crap.*` Lua API namespace (collections, globals, hooks, log, util,
//! crypto, schema).

mod access;
mod auth;
mod collections;
mod config;
mod crypto;
mod email;
mod env;
mod fields;
mod globals;
mod hooks;
mod http;
mod jobs;
mod log;
pub(crate) mod pages;
pub mod parse;
mod register;
pub(crate) mod richtext;
mod schema;
mod serializers;
pub(crate) mod template_data;
mod utils;
mod vm_label;

pub use register::register_api;
pub(crate) use serializers::{json_to_lua, lua_to_json};
pub use vm_label::VmLabel;
