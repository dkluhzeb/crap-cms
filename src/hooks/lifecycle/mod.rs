//! Hook execution engine: runs field, collection, and registered hooks within transactions.

pub mod access;
mod context;
pub(crate) mod converters;
mod execution;
mod runner;
mod types;
mod validation;

// Re-exports (preserves all existing external import paths)
pub(crate) use context::flatten_group_fields;
pub use context::{
    AccessCheckInput, AccessContext, AuthStrategyContext, AuthStrategyInput, ConditionContext,
    FieldHookContext, HookContext, HookContextBuilder, JobHandlerContext, JobInfo, ValidateContext,
};
pub use runner::{
    EventAfterReadInput, FieldHooksCall, FieldWriteCtx, HookRunner, HookRunnerBuilder,
    PublishEventInput,
};
pub use types::{DisplayConditionResult, FieldHookEvent, HookEvent, InitPhase, LuaCrudInfra};
// Internal types needed by sibling submodules (access.rs, context.rs)
// and by `lua_api/crud/` (the runtime CRUD layer was relocated there
// to sit alongside the rest of the `crap.*` registration code).
pub use execution::AfterReadCtx;
pub(crate) use execution::{
    apply_after_read_inner, resolve_hook_function, run_field_hooks_inner, run_hooks_inner,
};
pub(crate) use types::{
    DefaultDeny, HookDepth, HookDepthGuard, LuaInvalidationTransport, LuaLocaleConfig,
    LuaPopulateSingleflight, LuaStorage, MaxHookDepth, PoolContext, TxContext, UiLocaleContext,
    UserContext,
};
pub use validation::ValidationCtx;
pub use validation::is_valid_email_format;
pub(crate) use validation::richtext_attrs::run_before_validate_on_node_attrs;
pub(crate) use validation::validate_fields_inner;
