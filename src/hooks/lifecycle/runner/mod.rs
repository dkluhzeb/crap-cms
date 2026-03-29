//! HookRunner: thread-safe hook execution engine with a pool of Lua VMs.

mod access;
mod broadcast;
mod builder;
mod display;
mod field_write_ctx_builder;
mod jobs;
mod migrations;
mod publish_event_input_builder;
mod read_write;
mod run;
mod vm_pool;

pub use broadcast::PublishEventInput;
pub use builder::HookRunnerBuilder;
pub use run::FieldWriteCtx;

use vm_pool::VmPool;

use std::{collections::HashSet, sync::Arc};

use crate::core::registry::Registry;

/// Thread-safe hook runner with a pool of Lua VMs for concurrent execution.
#[derive(Clone)]
pub struct HookRunner {
    pool: Arc<VmPool>,
    /// Cached set of event names that have globally-registered hooks (from init.lua).
    /// Since hooks are only registered during VM creation (init.lua), this set is immutable.
    /// Allows skipping VM acquisition when no registered hooks exist for an event.
    registered_events: Arc<HashSet<String>>,
    /// Snapshot of the registry for richtext node attr validation.
    registry: Arc<Registry>,
}

impl HookRunner {
    /// Create a builder for constructing a HookRunner.
    pub fn builder() -> HookRunnerBuilder<'static> {
        HookRunnerBuilder::new()
    }

    /// Check if any globally-registered hooks exist for the given event.
    /// Uses the cached set — no VM acquisition needed.
    #[inline]
    pub fn has_registered_hooks_for(&self, event: &str) -> bool {
        self.registered_events.contains(event)
    }
}
