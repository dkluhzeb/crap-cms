//! Hook context types and Rust↔Lua marshalling.

mod access;
mod builder;
mod field_hook;
mod hook_context;
mod job;
mod strategy;
mod validate;

pub use access::AccessContext;
pub use builder::HookContextBuilder;
pub use field_hook::FieldHookContext;
pub use hook_context::HookContext;
pub(crate) use hook_context::flatten_group_fields;
pub use job::{JobHandlerContext, JobInfo};
pub use strategy::AuthStrategyContext;
pub use validate::ValidateContext;
