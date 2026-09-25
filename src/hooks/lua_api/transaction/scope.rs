//! The Lua-side scope of one write transaction, and its settlement.
//!
//! Every place a Lua VM owns a write transaction — `crap.transaction(fn)`, the
//! per-op transaction of a bare pool-mode CRUD call, a system transaction
//! (`on_init`, migrations), and an auth hook's lazily opened transaction —
//! opens a [`TxScope`] around the work and settles it through
//! [`TxScope::settle`]. The scope gives the transaction its own `crap.tx`
//! effect queue, event and verification queues, upload file-cleanup queue and
//! populate-cache dirty flag, and hands them up to the enclosing scope (or
//! publishes, deletes and clears) only after a commit.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    result::Result as StdResult,
};

use anyhow::{Error, Result};
use mlua::Lua;
use tracing::warn;

use crate::{
    core::{SharedCache, upload::delete_storage_keys},
    db::InPlaceTransaction,
    hooks::lifecycle::{FileCleanupQueue, LazyTx, LuaCrudInfra, LuaVmInfra, run_effects_on_vm},
    service::{DeferredEffect, DeferredQueue, EffectOutcome, EventQueue, VerificationQueue},
};

/// A transaction a [`TxScope`] settles: committed when the work succeeded,
/// rolled back when it is dropped.
pub(crate) trait ScopeTransaction {
    /// Commit the transaction, releasing its connection.
    ///
    /// # Errors
    ///
    /// Returns the commit error; the transaction is rolled back.
    fn commit(self) -> Result<()>;
}

impl ScopeTransaction for InPlaceTransaction<'_> {
    fn commit(self) -> Result<()> {
        InPlaceTransaction::commit(self)
    }
}

impl ScopeTransaction for LazyTx<'_> {
    fn commit(self) -> Result<()> {
        LazyTx::commit(self)
    }
}

/// Post-commit populate-cache invalidation for a transaction scope: hand the
/// dirty flag up to an enclosing scope if there is one, else clear the cache
/// now (the write is durable).
fn flush_cache_dirty(
    dirty: &Cell<bool>,
    outer: Option<&Rc<Cell<bool>>>,
    cache: Option<&SharedCache>,
) {
    if !dirty.get() {
        return;
    }

    if let Some(outer) = outer {
        outer.set(true);
    } else if let Some(cache) = cache
        && let Err(e) = cache.clear()
    {
        warn!("transaction scope: cache clear failed: {e:#}");
    }
}

/// The queues one scoped transaction owns, beside the enclosing scope's
/// queues they hand up to on commit (`None` = no enclosing scope).
///
/// FRESH event/verification queues are scoped to the transaction (frozen
/// contract: a rolled-back write never emits an event). The ambient job-level
/// queues flush unconditionally after the handler — routing inner-CRUD events
/// there directly would publish them even when this transaction rolls back.
/// On commit the events are handed up; on rollback they are dropped, exactly
/// like `run_pool_write`.
struct ScopedQueues {
    tx_events: EventQueue,
    outer_events: Option<EventQueue>,
    tx_verifications: VerificationQueue,
    outer_verifications: Option<VerificationQueue>,
    tx_files: FileCleanupQueue,
    outer_files: Option<FileCleanupQueue>,
    tx_cache_dirty: Rc<Cell<bool>>,
    outer_cache_dirty: Option<Rc<Cell<bool>>>,
    /// The cache handle, captured before `infra` moves into `app_data`, so the
    /// post-commit flush can clear it when there is no enclosing scope.
    cache: Option<SharedCache>,
}

impl ScopedQueues {
    /// Swap fresh per-transaction queues into `infra`, keeping the enclosing
    /// scope's queues to hand up to on commit.
    fn install(infra: &mut LuaCrudInfra) -> Self {
        let tx_events: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let tx_verifications: VerificationQueue = Rc::new(RefCell::new(Vec::new()));
        let tx_files: FileCleanupQueue = Rc::new(RefCell::new(Vec::new()));
        let tx_cache_dirty: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        Self {
            outer_events: infra.event_queue.replace(tx_events.clone()),
            outer_verifications: infra.verification_queue.replace(tx_verifications.clone()),
            outer_files: infra.file_cleanup.replace(tx_files.clone()),
            outer_cache_dirty: infra.cache_dirty.replace(tx_cache_dirty.clone()),
            cache: infra.cache.clone(),
            tx_events,
            tx_verifications,
            tx_files,
            tx_cache_dirty,
        }
    }

    /// The transaction committed: hand its events, verifications, upload
    /// files and cache-dirty flag up to the enclosing scope — or, with none,
    /// publish, delete and clear now. Needs no database connection.
    fn settle_after_commit(&self, lua: &Lua, label: &str) {
        self.hand_up_events(label);
        self.hand_up_verifications(label);
        self.settle_files(lua);

        flush_cache_dirty(
            &self.tx_cache_dirty,
            self.outer_cache_dirty.as_ref(),
            self.cache.as_ref(),
        );
    }

    /// Hand the transaction's events up to the ambient queue, which flushes
    /// once the surface's VM work is done.
    fn hand_up_events(&self, label: &str) {
        if let Some(outer) = &self.outer_events {
            outer
                .borrow_mut()
                .extend(self.tx_events.borrow_mut().drain(..));

            return;
        }

        if !self.tx_events.borrow().is_empty() {
            warn!(
                "{label}: {} event(s) from a committed transaction had no ambient queue \
                 to flush into and were dropped",
                self.tx_events.borrow().len()
            );
        }
    }

    /// Hand the transaction's pending verifications up to the enclosing
    /// service write, which sends them after its own commit.
    ///
    /// A verification only lands in this queue when the write had no email
    /// context to issue it in-transaction; every surface that owns its
    /// transaction (jobs, routes, migrations, `on_init`, auth hooks) carries
    /// one, so with no enclosing scope the queue stays empty. Should anything
    /// arrive here anyway it is reported, never dropped silently — an account
    /// without its verification can neither verify nor sign up again.
    fn hand_up_verifications(&self, label: &str) {
        if let Some(outer) = &self.outer_verifications {
            outer
                .borrow_mut()
                .extend(self.tx_verifications.borrow_mut().drain(..));

            return;
        }

        for pending in self.tx_verifications.borrow_mut().drain(..) {
            warn!(
                "{label}: account {} in '{}' requires email verification, but this \
                 transaction has no email context to issue it through — no \
                 verification was sent",
                pending.doc_id, pending.slug
            );
        }
    }

    /// Upload files from hard deletes inside this transaction: hand up to an
    /// enclosing scope, or — with none — delete NOW (we are post-commit) via
    /// the VM's storage handle.
    fn settle_files(&self, lua: &Lua) {
        if let Some(outer) = &self.outer_files {
            outer
                .borrow_mut()
                .extend(self.tx_files.borrow_mut().drain(..));

            return;
        }

        let Some(storage) = lua
            .app_data_ref::<LuaVmInfra>()
            .and_then(|i| i.storage.clone())
        else {
            return;
        };

        let keys: Vec<String> = self.tx_files.borrow_mut().drain(..).collect();
        delete_storage_keys(&*storage, &keys);
    }
}

/// RAII restore for the transaction-scoped [`LuaCrudInfra`] swap. On
/// drop — return OR unwind — the previous infra (or absence) is put back.
/// A Rust panic unwinding out of a `crap.*` callback must not return the VM
/// to the pool with the transaction-scoped infra (deferred queue, caches)
/// still installed: a later request would then see a live deferred queue
/// outside any transaction, and `crap.tx.on_commit` would silently no-op
/// into it.
struct InfraRestore<'a> {
    lua: &'a Lua,
    /// The infra to put back on drop; `None` = there was none before, so
    /// the slot is removed.
    prev: Option<LuaCrudInfra>,
}

impl Drop for InfraRestore<'_> {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(p) => {
                self.lua.set_app_data(p);
            }
            None => {
                self.lua.remove_app_data::<LuaCrudInfra>();
            }
        }
    }
}

/// The Lua-side scope of one write transaction (see the module docs).
///
/// Opening it swaps a transaction-scoped [`LuaCrudInfra`] — the current one
/// with fresh queues — into the VM; [`Self::settle`] puts the enclosing one
/// back, resolves the transaction and delivers what the work queued. Dropped
/// unsettled (an unwind), it only restores the enclosing infra.
pub(crate) struct TxScope<'a> {
    lua: &'a Lua,
    label: &'a str,
    deferred: DeferredQueue,
    queues: ScopedQueues,
    restore: Option<InfraRestore<'a>>,
}

impl<'a> TxScope<'a> {
    /// Open the scope on `lua`; `label` prefixes its log lines.
    pub(crate) fn open(lua: &'a Lua, label: &'a str) -> Self {
        let deferred: DeferredQueue = Rc::new(RefCell::new(Vec::new()));
        let prev = lua.app_data_ref::<LuaCrudInfra>().map(|r| (*r).clone());

        let mut infra = prev.clone().unwrap_or_default();
        infra.deferred = Some(deferred.clone());

        let queues = ScopedQueues::install(&mut infra);
        lua.set_app_data(infra);

        Self {
            lua,
            label,
            deferred,
            queues,
            restore: Some(InfraRestore { lua, prev }),
        }
    }

    /// Settle the scope's transaction `tx` once its work produced `result`.
    ///
    /// With `commit` set, `tx` is committed; its queued events,
    /// verifications, file deletions and cache invalidation are delivered
    /// (or handed up), and the `crap.tx.on_commit` effects run. Otherwise —
    /// or when the commit fails — `tx` is rolled back, the queues die with
    /// it, and the `on_rollback` compensations run.
    ///
    /// The connection is released before any effect runs, on both paths: an
    /// effect's own CRUD opens a scoped transaction on a fresh write-pool
    /// checkout, which must not have to wait for the slot this scope held.
    /// Effects run in this VM, in whatever database context encloses the
    /// scope (pool-mode for a job, a route or an auth hook).
    ///
    /// # Errors
    ///
    /// Returns `result`'s own error, or the commit error through
    /// `commit_err`.
    pub(crate) fn settle<R, E>(
        mut self,
        tx: impl ScopeTransaction,
        result: StdResult<R, E>,
        commit: bool,
        commit_err: impl FnOnce(Error) -> E,
    ) -> StdResult<R, E> {
        // The work is done: the enclosing infra goes back before anything
        // else runs, so effects never register into this spent scope.
        self.restore.take();

        let effects: Vec<DeferredEffect> = self.deferred.borrow_mut().drain(..).collect();

        if !commit {
            drop(tx);
            run_effects_on_vm(self.lua, &effects, EffectOutcome::Rollback);

            return result;
        }

        if let Err(e) = tx.commit() {
            // A failed commit is a rollback outcome — the queued
            // events/verifications die with the transaction.
            run_effects_on_vm(self.lua, &effects, EffectOutcome::Rollback);

            return Err(commit_err(e));
        }

        self.queues.settle_after_commit(self.lua, self.label);
        run_effects_on_vm(self.lua, &effects, EffectOutcome::Commit);

        result
    }
}
