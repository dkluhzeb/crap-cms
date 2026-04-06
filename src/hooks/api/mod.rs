//! Registers the `crap.*` Lua API namespace (collections, globals, hooks, log, util,
//! crypto, schema).

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
pub mod parse;
mod register;
pub(crate) mod richtext;
mod schema;
mod serializers;
mod utils;

pub use register::register_api;
pub(crate) use serializers::{json_to_lua, lua_to_json};

/// Label stored in `Lua::app_data` to identify which VM is logging.
/// Init VM uses `"init"`, pool VMs use `"vm-1"`, `"vm-2"`, etc.
pub struct VmLabel(pub String);
