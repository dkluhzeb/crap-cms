//! Hook execution engine: runs field, collection, and registered hooks within transactions.

pub mod crud;
pub mod access;
pub(crate) mod converters;
mod context;
mod execution;
mod runner;
mod types;
mod validation;

// Re-exports (preserves all existing external import paths)
pub use context::{HookContext, HookContextBuilder};
pub use runner::{HookRunner, HookRunnerBuilder};
pub use types::{DisplayConditionResult, HookEvent, FieldHookEvent};
// Internal types needed by sibling submodules (crud.rs, access.rs, context.rs).
pub(crate) use types::{TxContext, UserContext, UiLocaleContext, HookDepth, MaxHookDepth, DefaultDeny};
pub use validation::evaluate_condition_table;
