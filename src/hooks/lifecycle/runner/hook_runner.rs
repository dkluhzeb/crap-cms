//! `HookRunner` — thread-safe hook execution engine with a pool of Lua VMs.

use std::{collections::HashSet, sync::Arc};

use crate::core::Registry;

use super::HookRunnerBuilder;
use super::vm_pool::VmPool;

/// Thread-safe hook runner with a pool of Lua VMs for concurrent execution.
#[derive(Clone)]
pub struct HookRunner {
    pub(super) pool: Arc<VmPool>,
    /// Cached set of event names that have globally-registered hooks (from init.lua).
    /// Since hooks are only registered during VM creation (init.lua), this set is immutable.
    /// Allows skipping VM acquisition when no registered hooks exist for an event.
    pub(super) registered_events: Arc<HashSet<String>>,
    /// Snapshot of the registry for richtext node attr validation.
    pub(super) registry: Arc<Registry>,
    /// Snapshot of `[access] default_deny` so [`check_access`] can
    /// answer "no access function configured" without acquiring a
    /// Lua VM from the pool. With pool size 16 and bench concurrency
    /// 50, the unconditional `pool.acquire()` previously serialized
    /// 34 out of 50 concurrent reads on the VM-pool mutex — dwarfed
    /// every other allocation cost in the profile.
    ///
    /// [`check_access`]: Self::check_access
    pub(super) default_deny: bool,
}

impl HookRunner {
    /// Create a builder for constructing a `HookRunner`.
    #[must_use]
    pub fn builder() -> HookRunnerBuilder<'static> {
        HookRunnerBuilder::new()
    }

    /// Check if any globally-registered hooks exist for the given event.
    /// Uses the cached set — no VM acquisition needed.
    #[inline]
    #[must_use]
    pub fn has_registered_hooks_for(&self, event: &str) -> bool {
        self.registered_events.contains(event)
    }
}
