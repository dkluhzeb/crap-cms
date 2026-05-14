//! Lua VM setup, the `crap.*` API surface exposed to user scripts, and
//! the lifecycle layer that runs Lua hooks around CRUD/access/access
//! events.
//!
//! ## Submodule layout
//!
//! - `init.rs` -- VM creation (`init_lua`), config-dir loading
//!   (`load_lua_dir`), and the sandbox restrictions (`sandbox_lua`)
//!   applied to every hook VM.
//! - `lua_api/` -- the `crap.*` API surface registered into each VM:
//!   collections / globals / jobs CRUD, http, email, cache, fields,
//!   richtext, json, log, version, etc. One file per `crap.<area>`.
//! - `startup_checks.rs` -- post-init correctness passes that walk
//!   the registry once at boot: every statically-known hook/access
//!   ref must resolve in the Lua VM, and no field name may collide
//!   with the generated `{name}__{locale}` column pattern. Distinct
//!   from `lifecycle/validation/`, which runs per-write field
//!   validation.
//! - `lifecycle/` -- runtime hook execution. `HookRunner` owns a Lua
//!   VM pool; `HookEvent` enumerates the events that fire user hooks
//!   (`before_validate`, `before_change`, `after_change`, `before_read`,
//!   `after_read`, `before_delete`, `after_delete`, `before_broadcast`); the
//!   per-event execution paths live under `lifecycle/execution/`,
//!   `lifecycle/access/`, `lifecycle/validation/`, and
//!   `lifecycle/runner/`.
//!
//! ## Conventions
//!
//! - Two VM flavours: `init_lua` (startup) loads user code once;
//!   `HookRunner` (runtime) maintains a pool of VMs that each
//!   request borrows.
//! - `run_hooks_with_conn` uses a `TxContext` fat-pointer pattern
//!   (`lua.set_app_data`) to thread a borrowed `&dyn DbConnection`
//!   into Lua-callable CRUD without violating mlua's `'static` API
//!   constraints.
//! - Wide-arg recursive helpers use the walker pattern
//!   (`FieldHookWalker`, `ValidationWalker`) instead of >4 positional
//!   args.

mod init;
pub mod lifecycle;
pub mod lua_api;
mod startup_checks;

pub use init::init_lua;
pub(crate) use init::{load_lua_dir, sandbox_lua};
pub use lifecycle::{
    DisplayConditionResult, HookContext, HookEvent, HookRunner, LuaCrudInfra, ValidationCtx,
};
