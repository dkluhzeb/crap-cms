//! A Lua VM with `crap.fields` and the init-phase `crap.richtext` installed,
//! in the init phase — shared by the registration and rendering tests.

use std::sync::Arc;

use mlua::Lua;

use super::api::register_richtext_init;
use crate::{
    core::{Registry, SharedRegistry},
    hooks::{lifecycle::InitPhase, lua_api::fields::register_fields},
};

pub(super) fn setup_lua() -> (Lua, SharedRegistry) {
    let lua = Lua::new();
    let registry = Registry::shared();
    let crap = lua.create_table().unwrap();
    lua.globals().set("crap", crap).unwrap();
    register_fields(&lua).unwrap();
    register_richtext_init(&lua, Arc::clone(&registry)).unwrap();

    // Mimic init-time loading so register_node accepts the call.
    lua.set_app_data(InitPhase);
    (lua, registry)
}
