//! `HookRunner` execution of transaction-outcome effects
//! (`crap.tx.on_commit` / `crap.tx.on_rollback`).
//!
//! Effects run **after** the originating transaction resolved, in
//! **pool-mode** (like a job handler): each Lua CRUD call inside an effect
//! opens its own short-lived IMMEDIATE transaction. Effect errors are logged
//! and skipped — the transaction outcome is final, so effects are fail-open
//! by design (documented in `docs/src/hooks/transaction-access.md`).

use std::{cell::RefCell, rc::Rc};

use mlua::Lua;
use tracing::warn;

use crate::{
    db::DbPool,
    hooks::{
        HookRunner, LuaCrudInfra,
        lifecycle::{execution::resolve_hook_function, types::TxContextGuard},
        lua_api,
    },
    service::{DeferredEffect, EffectOutcome, EventQueue, ServiceContext, flush_queue},
};

/// Call every effect bound to `outcome` on the given VM. The caller is
/// responsible for the VM's context (`TxContextGuard`) — this only resolves
/// and invokes. Errors are logged per effect and never propagate.
pub(crate) fn run_effects_on_vm(lua: &Lua, effects: &[DeferredEffect], outcome: EffectOutcome) {
    for effect in effects.iter().filter(|e| e.outcome == outcome) {
        if let Err(e) = call_one_effect(lua, effect, outcome) {
            warn!(
                "tx {} effect '{}' failed: {e:#}",
                outcome.as_str(),
                effect.hook_ref
            );
        }
    }
}

/// Resolve one effect's hook ref and call it with `{ data, outcome }`.
fn call_one_effect(
    lua: &Lua,
    effect: &DeferredEffect,
    outcome: EffectOutcome,
) -> anyhow::Result<()> {
    let func = resolve_hook_function(lua, &effect.hook_ref)?;

    let ctx = lua.create_table()?;
    ctx.set("data", lua_api::json_to_lua(lua, &effect.payload)?)?;
    ctx.set("outcome", outcome.as_str())?;

    func.call::<()>(ctx)?;

    Ok(())
}

impl HookRunner {
    /// Run transaction-outcome effects post-transaction, in pool-mode.
    ///
    /// Acquires a fresh pool VM with a job-handler-style context: pool-mode
    /// CRUD (per-op transactions), no user identity, and a fresh event queue
    /// so events published by effect CRUD flush after all effects ran (the
    /// same post-handler flush model as `run_job_handler`).
    pub fn run_deferred_effects(
        &self,
        pool: &DbPool,
        ctx: &ServiceContext,
        effects: &[DeferredEffect],
        outcome: EffectOutcome,
    ) {
        let event_queue: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let mut infra = LuaCrudInfra::from_ctx(ctx, Some(event_queue.clone()), None);
        // Effects cannot re-register: their transaction is already resolved.
        infra.deferred = None;

        self.run_effects_in_vm(pool, infra, effects, outcome);

        let flush_ctx = ServiceContext::slug_only("")
            .runner(self)
            .event_transport(ctx.event_transport.clone())
            .build();
        flush_queue(&flush_ctx, &event_queue);
    }

    /// The VM-holding body of [`Self::run_deferred_effects`] — split out so
    /// the VM lease is released before the post-effect event flush (whose
    /// `before_broadcast` hooks acquire their own VM).
    fn run_effects_in_vm(
        &self,
        pool: &DbPool,
        infra: LuaCrudInfra,
        effects: &[DeferredEffect],
        outcome: EffectOutcome,
    ) {
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                warn!("VM pool error running deferred tx effects: {e:#}");
                return;
            }
        };

        let _guard = TxContextGuard::set_pool(&lua, pool.clone(), None, None, Some(infra));

        run_effects_on_vm(&lua, effects, outcome);
    }
}
