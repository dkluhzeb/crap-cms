//! Lua VM setup, `crap.*` API registration, and hook lifecycle management.

pub mod api;
mod init;
pub mod lifecycle;
mod validate;

pub use init::init_lua;
pub(crate) use init::{load_lua_dir, sandbox_lua};
pub use lifecycle::{HookContext, HookEvent, HookRunner, LuaCrudInfra, ValidationCtx};
pub use validate::{validate_hook_references, validate_locale_field_collisions};
