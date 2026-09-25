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
        lifecycle::{
            execution::resolve_hook_function,
            types::{ExecutionDeadlineGuard, TxContextGuard},
        },
        lua_api,
    },
    service::{DeferredEffect, EffectOutcome, EventQueue, ServiceContext, flush_queue},
};

/// Call every effect bound to `outcome` on the given VM. The caller is
/// responsible for the VM's context (`TxContextGuard`) — this only resolves
/// and invokes. Errors are logged per effect and never propagate.
///
/// A job handler's deadline is lifted while the effects run: they belong to
/// a transaction that already resolved, so a job stopped at its timeout
/// still delivers a committed write's side effects and a rolled-back one's
/// compensations.
pub(crate) fn run_effects_on_vm(lua: &Lua, effects: &[DeferredEffect], outcome: EffectOutcome) {
    let _deadline = ExecutionDeadlineGuard::suspend(lua);

    for effect in effects.iter().filter(|e| e.runs_on(outcome)) {
        if let Err(e) = call_one_effect(lua, effect) {
            warn!(
                "tx {} effect '{}' failed: {e:#}",
                effect.outcome.as_str(),
                effect.hook_ref
            );
        }
    }
}

/// Resolve one effect's hook ref and call it with `{ data, outcome }`, where
/// `outcome` is the one the effect was registered for.
fn call_one_effect(lua: &Lua, effect: &DeferredEffect) -> anyhow::Result<()> {
    let func = resolve_hook_function(lua, &effect.hook_ref)?;

    let ctx = lua.create_table()?;
    ctx.set("data", lua_api::json_to_lua(lua, &effect.payload)?)?;
    ctx.set("outcome", effect.outcome.as_str())?;

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::hooks::lifecycle::types::{ExecutionDeadline, check_execution_deadline};

    /// Regression guard: a job stopped at its deadline must still run the
    /// effects of a transaction that already resolved — a committed write's
    /// side effect would otherwise be lost. The deadline is back afterwards.
    #[test]
    fn effects_run_even_after_the_job_deadline() {
        let lua = Lua::new();

        // Stands in for every deadline-checked entry point (CRUD, HTTP, email).
        let probe = lua
            .create_function(|lua, ()| check_execution_deadline(lua))
            .expect("probe");
        lua.globals().set("probe", probe).expect("set probe");

        lua.load(
            r#"package.loaded["hooks.effect"] = function(ctx)
                   probe()
                   effect_ran = ctx.outcome
               end"#,
        )
        .exec()
        .expect("register effect");
        let _deadline = ExecutionDeadlineGuard::install(&lua, ExecutionDeadline::new(0));

        let effects = [DeferredEffect {
            outcome: EffectOutcome::Commit,
            hook_ref: "hooks.effect".to_string(),
            payload: json!({}),
            unconditional: false,
        }];
        run_effects_on_vm(&lua, &effects, EffectOutcome::Commit);

        let ran: Option<String> = lua.globals().get("effect_ran").expect("global");
        assert_eq!(ran.as_deref(), Some("commit"));
        assert!(
            check_execution_deadline(&lua).is_err(),
            "the deadline is restored once the effects have run"
        );
    }
}
