//! Hook context types and Rust↔Lua marshalling.

mod access;
mod builder;
mod condition;
mod field_hook;
mod hook_context;
mod job;
mod live;
pub(crate) mod operation;
mod route;
mod strategy;
mod validate;

pub use access::{AccessCheckInput, AccessContext};
pub use builder::HookContextBuilder;
pub use condition::ConditionContext;
pub use field_hook::FieldHookContext;
pub use hook_context::HookContext;
pub(crate) use hook_context::hook_result_slot;
pub use job::{JobHandlerContext, JobInfo};
pub use live::LiveFilterContext;
pub use route::{RouteContext, RouteHandlerInput};
pub use strategy::{
    AuthStrategyContext, AuthStrategyInput, MfaDeliverContext, MfaDeliverInput, MfaWhenContext,
    MfaWhenInput,
};
pub use validate::ValidateContext;
