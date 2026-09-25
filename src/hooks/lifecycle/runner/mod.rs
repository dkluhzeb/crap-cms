//! `HookRunner`: thread-safe hook execution engine with a pool of Lua VMs.

mod access;
mod auth;
mod broadcast;
mod builder;
mod deferred;
mod display;
mod hook_runner;
mod jobs;
mod migrations;
mod read_write;
mod routes;
mod run;
mod system_tx;
mod vm_pool;

pub use broadcast::PublishEventInput;
pub use builder::HookRunnerBuilder;
pub(crate) use deferred::run_effects_on_vm;
pub use display::{RenderCrud, RenderInfo, RenderParams};
pub use hook_runner::HookRunner;
pub use migrations::MigrationCall;
pub use read_write::EventAfterReadInput;
pub use run::{FieldHooksCall, FieldWriteCtx};
pub use vm_pool::VmPoolExhausted;
pub(crate) use vm_pool::{apply_vm_limits, reset_instruction_budget};
