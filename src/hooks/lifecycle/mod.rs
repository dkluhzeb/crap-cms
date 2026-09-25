//! Hook execution engine: runs field, collection, and registered hooks within transactions.

pub mod access;
mod context;
pub(crate) mod converters;
mod execution;
mod lazy_tx;
mod runner;
mod types;
mod validation;

// Re-exports (preserves all existing external import paths)
pub(crate) use context::operation;
pub use context::{
    AccessCheckInput, AccessContext, AuthStrategyContext, AuthStrategyInput, ConditionContext,
    FieldHookContext, HookContext, HookContextBuilder, JobHandlerContext, JobInfo,
    LiveFilterContext, MfaDeliverContext, MfaDeliverInput, MfaWhenContext, MfaWhenInput,
    RouteContext, RouteHandlerInput, ValidateContext,
};
pub use runner::{
    EventAfterReadInput, FieldHooksCall, FieldWriteCtx, HookRunner, HookRunnerBuilder,
    MigrationCall, PublishEventInput, RenderCrud, RenderInfo, RenderParams, VmPoolExhausted,
};
pub(crate) use runner::{apply_vm_limits, reset_instruction_budget, run_effects_on_vm};
pub use types::{
    DisplayConditionResult, FieldHookEvent, FileCleanupQueue, HookEvent, InitPhase, LuaCrudInfra,
};
// Internal types needed by sibling submodules (access.rs, context.rs)
// and by `lua_api/crud/` (the runtime CRUD layer was relocated there
// to sit alongside the rest of the `crap.*` registration code).
pub use execution::AfterReadCtx;
pub(crate) use execution::{
    FieldHookMeta, apply_after_read_inner, resolve_hook_function, run_field_hooks_inner,
    run_hooks_inner,
};
pub(crate) use lazy_tx::{LazyTx, LazyTxContext, LazyTxGuard};
pub(crate) use types::{
    AfterReadScope, AfterReadScopeGuard, ExecutionDeadline, ExecutionDeadlineGuard, HookDepth,
    HookDepthGuard, LuaVmInfra, PoolContext, PoolMode, ReadOnlyScope, ReadOnlyScopeGuard,
    TxContext, TxContextGuard, UiLocaleContext, UserContext, check_execution_deadline,
    execution_time_left,
};
pub use validation::ValidationCtx;
pub use validation::is_valid_email_format;
pub(crate) use validation::richtext_attrs::apply_node_attr_before_validate;
pub(crate) use validation::validate_write_fields;
