//! Lua table serializers for CollectionDefinition, GlobalDefinition, and FieldDefinition.
//! These produce round-trip compatible tables that can be passed back to
//! `crap.collections.define()` / `crap.globals.define()`.

mod collection;
mod fields;
mod auth;
mod upload;
mod admin;
mod helpers;

pub(super) use collection::{collection_config_to_lua, global_config_to_lua};
pub(crate) use helpers::{lua_to_json, json_to_lua};
