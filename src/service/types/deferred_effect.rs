//! Transaction-outcome effect queued during a write transaction
//! (`crap.tx.on_commit` / `crap.tx.on_rollback`), run after the transaction
//! resolves.
//!
//! Mirrors the [`super::pending_event`] model: hooks running inside the write
//! transaction register effects into a shared per-transaction queue; the
//! pool-write envelope (`run_pool_write`) or `crap.transaction(fn)` drains it
//! once the transaction outcome is known. Effects are **hook refs + plain
//! JSON payloads** — never Lua closures — because they execute later in a
//! different (or re-contextualized) pooled VM, in pool-mode (each CRUD call in
//! the effect opens its own short-lived transaction, like a job handler).

use std::{cell::RefCell, rc::Rc};

use serde_json::Value as JsonValue;
use tracing::warn;

use crate::service::ServiceContext;

/// Which transaction outcome an effect is bound to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EffectOutcome {
    /// Run only after the transaction committed successfully.
    Commit,
    /// Run only after the transaction rolled back (body error or failed commit).
    Rollback,
}

impl EffectOutcome {
    /// The Lua-facing name, surfaced to effect handlers as `ctx.outcome`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Rollback => "rollback",
        }
    }
}

/// One deferred effect: a resolvable hook ref plus the JSON payload captured
/// at registration time. Both are validated when `crap.tx.on_commit` /
/// `on_rollback` is called (unresolvable ref or unserializable payload fails
/// the registering hook — and with it the transaction — fail-closed).
pub struct DeferredEffect {
    pub outcome: EffectOutcome,
    pub hook_ref: String,
    pub payload: JsonValue,
}

/// Shared per-transaction queue of deferred effects.
/// Cloning is cheap (Rc + `RefCell`); same-thread-only like [`super::pending_event::EventQueue`].
pub type DeferredQueue = Rc<RefCell<Vec<DeferredEffect>>>;

/// Drain the queue and run the effects bound to `outcome`; effects bound to
/// the other outcome are dropped (their transaction resolved the other way).
///
/// Runs post-transaction: effect errors are logged and skipped (fail-open —
/// the transaction outcome is already final and cannot be revisited).
pub(crate) fn flush_deferred_effects(
    ctx: &ServiceContext,
    queue: &DeferredQueue,
    outcome: EffectOutcome,
) {
    let effects: Vec<DeferredEffect> = queue.borrow_mut().drain(..).collect();
    let effects: Vec<DeferredEffect> = effects
        .into_iter()
        .filter(|e| e.outcome == outcome)
        .collect();

    if effects.is_empty() {
        return;
    }

    let Some(runner) = ctx.runner else {
        warn!("deferred tx effects dropped: no hook runner on the context");
        return;
    };
    let Some(pool) = ctx.pool else {
        warn!("deferred tx effects dropped: no pool on the context");
        return;
    };

    runner.run_deferred_effects(pool, ctx, &effects, outcome);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Lua-facing outcome names are contract (`ctx.outcome`).
    #[test]
    fn outcome_names() {
        assert_eq!(EffectOutcome::Commit.as_str(), "commit");
        assert_eq!(EffectOutcome::Rollback.as_str(), "rollback");
    }
}
