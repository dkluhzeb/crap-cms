//! Registers the `crap.*` Lua API namespace exposed to user hook scripts:
//! collections / globals / hooks / log / util / crypto / schema / etc.
//!
//! ## Submodule layout
//!
//! - One file per `crap.<area>` for the simple, no-DB-transaction
//!   bits (auth, cache, crypto, email, env, fields, http, log, pages,
//!   richtext, schema, `template_data`, `vm_label`, …).
//! - **`crud/`** holds the runtime CRUD surface (`crap.collections.find`,
//!   `crap.collections.create`, `crap.globals.update`, …) — the bits
//!   that need the active transaction. They depend on the
//!   `TxContext` machinery in `lifecycle/` (set by
//!   `run_hooks_with_conn`) but conceptually belong here alongside the
//!   rest of `crap.*` registration; they're physically grouped here
//!   so a contributor looking up "where is `crap.collections.find()`
//!   defined" lands in one tree.
//! - **`parse/`** parses Lua tables → typed Rust definitions
//!   (collection / global / job definitions, fields, blocks, etc.).
//! - **`serializers/`** is the inverse — typed Rust → Lua tables.
//! - **`register.rs`** is the single entry point called from
//!   `init_lua` and `HookRunner`; it walks the per-module register
//!   functions in fixed order.

pub(crate) mod access;
pub(crate) mod auth;
pub(crate) mod cache;
pub(crate) mod collections;
pub(crate) mod config;
pub(crate) mod crud;
pub(crate) mod crypto;
pub(crate) mod email;
pub(crate) mod env;
pub(crate) mod fields;
pub(crate) mod globals;
pub(crate) mod hooks;
pub(crate) mod http;
pub(crate) mod jobs;
pub(crate) mod log;
pub mod pages;
pub mod parse;
pub(crate) mod register;
pub(crate) mod richtext;
pub(crate) mod routes;
pub(crate) mod schema;
mod serializers;
pub(crate) mod storage;
pub(crate) mod template_data;
pub(crate) mod transaction;
pub(crate) mod utils;
mod vm_label;

pub use register::{register_api, register_api_pool_init};
pub(crate) use serializers::{json_to_lua, lua_to_json, set_max_nesting_depth};
pub use vm_label::VmLabel;
