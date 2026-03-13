//! Hook execution engine: runs field, collection, and registered hooks within transactions.

pub mod access;
mod context;
pub(crate) mod converters;
pub mod crud;
mod execution;
mod runner;
mod types;
mod validation;

// Re-exports (preserves all existing external import paths)
pub use context::{HookContext, HookContextBuilder};
pub use runner::{HookRunner, HookRunnerBuilder};
pub use types::{DisplayConditionResult, FieldHookEvent, HookEvent};
// Internal types needed by sibling submodules (crud.rs, access.rs, context.rs).
pub use execution::AfterReadCtx;
pub(crate) use types::{
    DefaultDeny, HookDepth, MaxHookDepth, TxContext, UiLocaleContext, UserContext,
};
pub use validation::ValidationCtx;
pub use validation::evaluate_condition_table;
