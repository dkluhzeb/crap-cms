//! Lua VM setup, `crap.*` API registration, and hook lifecycle management.

pub mod api;
mod init;
pub mod lifecycle;
mod validate;

pub use init::init_lua;
pub(crate) use init::{load_lua_dir, sandbox_lua};
pub use lifecycle::{
    DisplayConditionResult, HookContext, HookEvent, HookRunner, LuaCrudInfra, ValidationCtx,
};
