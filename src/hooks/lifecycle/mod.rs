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
pub use runner::{FieldWriteCtx, HookRunner, HookRunnerBuilder, PublishEventInput};
pub use types::{DisplayConditionResult, FieldHookEvent, HookEvent, LuaCrudInfra};
// Internal types needed by sibling submodules (crud.rs, access.rs, context.rs).
pub use execution::AfterReadCtx;
pub(crate) use execution::{
    apply_after_read_inner, resolve_hook_function, run_field_hooks_inner, run_hooks_inner,
};
pub(crate) use types::{
    DefaultDeny, HookDepth, HookDepthGuard, LuaInvalidationTransport, LuaPopulateSingleflight,
    LuaStorage, MaxHookDepth, TxContext, UiLocaleContext, UserContext,
};
pub use validation::ValidationCtx;
pub use validation::evaluate_condition_table;
pub use validation::is_valid_email_format;
pub(crate) use validation::richtext_attrs::run_before_validate_on_node_attrs;
pub(crate) use validation::validate_fields_inner;
