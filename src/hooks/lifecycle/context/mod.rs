//! Hook context types and Rust↔Lua marshalling.

mod access;
mod builder;
mod condition;
mod field_hook;
mod hook_context;
mod job;
mod strategy;
mod validate;

pub use access::{AccessCheckInput, AccessContext};
pub use builder::HookContextBuilder;
pub use condition::ConditionContext;
pub use field_hook::FieldHookContext;
pub use hook_context::HookContext;
pub(crate) use hook_context::flatten_group_fields;
pub use job::{JobHandlerContext, JobInfo};
pub use strategy::{AuthStrategyContext, AuthStrategyInput};
pub use validate::ValidateContext;
