//! Lua table serializers for CollectionDefinition, GlobalDefinition, and FieldDefinition.
//! These produce round-trip compatible tables that can be passed back to
//! `crap.collections.define()` / `crap.globals.define()`.

mod admin;
mod auth;
mod collection;
mod fields;
mod global;
mod helpers;
mod upload;

pub(super) use collection::collection_config_to_lua;
pub(super) use global::global_config_to_lua;
pub(crate) use helpers::{json_to_lua, lua_to_json};
