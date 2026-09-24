//! The transaction scope system-level Lua runs in: data migrations and
//! `on_init` hooks.
//!
//! Both run Lua CRUD outside any service write envelope, so they need the
//! same transaction scope every other Lua write gets — otherwise a hard
//! delete of an upload document inside them would remove its files before
//! the transaction commits (a rollback then restores rows whose files are
//! gone), the populate cache would never be invalidated, and no live event
//! would ever be published. The scope is the one `crap.transaction(fn)` and a
//! bare pool-mode CRUD call already use ([`run_scoped_tx`]): per-transaction
//! event / file-cleanup / cache-dirty / `crap.tx` queues, settled only after a
//! successful commit and dropped on rollback.

use anyhow::{Error, Result, anyhow};
use mlua::{Error::RuntimeError, Lua};

use crate::{
    db::{DbConnection, DbPool},
    hooks::{
        HookRunner, LuaCrudInfra, lifecycle::TxContextGuard, lua_api::transaction::run_scoped_tx,
    },
    service::{EventQueue, ServiceContext, flush_queue},
};

impl HookRunner {
    /// Run `work` on one pooled VM inside ONE write transaction carrying the
    /// full Lua CRUD transaction scope, and commit it when `work` succeeds.
    ///
    /// `infra` carries the event transport and the populate cache the scope
    /// settles into after the commit: events written inside the transaction
    /// are published, the cache is cleared, and upload files of hard-deleted
    /// documents are removed — all only once the transaction is durable. A
    /// failure rolls everything back and leaves the files in place (an
    /// orphaned file is the safe direction). `label` names the scope in
    /// transaction errors.
    ///
    /// Events are published after the VM is released: publishing runs
    /// `before_broadcast` hooks, which acquire a VM of their own.
    ///
    /// # Errors
    ///
    /// Returns `work`'s own error unchanged, or an error when no VM can be
    /// acquired or the transaction cannot be opened or committed.
    pub(super) fn run_in_system_tx(
        &self,
        pool: &DbPool,
        infra: Option<LuaCrudInfra>,
        label: &str,
        work: impl FnOnce(&Lua, &dyn DbConnection) -> Result<()>,
    ) -> Result<()> {
        let events: EventQueue = EventQueue::default();
        let event_transport = infra.as_ref().and_then(|i| i.event_transport.clone());

        let mut infra = infra.unwrap_or_default();
        infra.event_queue = Some(events.clone());

        let result = self.run_system_tx_in_vm(pool, infra, label, work);

        // Only a committed transaction hands its events up to this queue, so
        // flushing unconditionally never publishes a rolled-back write.
        let flush_ctx = ServiceContext::slug_only("")
            .runner(self)
            .event_transport(event_transport)
            .build();
        flush_queue(&flush_ctx, &events);

        result
    }

    /// The VM-holding body of [`Self::run_in_system_tx`].
    fn run_system_tx_in_vm(
        &self,
        pool: &DbPool,
        infra: LuaCrudInfra,
        label: &str,
        work: impl FnOnce(&Lua, &dyn DbConnection) -> Result<()>,
    ) -> Result<()> {
        let vm = self.pool.acquire()?;
        let lua: &Lua = &vm;
        let _guard = TxContextGuard::set_pool(lua, pool.clone(), None, None, Some(infra));

        // `run_scoped_tx` speaks Lua errors; `work`'s own error is kept here
        // so it reaches the caller with its full context chain.
        let mut failure: Option<Error> = None;

        let scoped = run_scoped_tx(lua, label, |conn| {
            work(lua, conn).map_err(|e| {
                let message = format!("{e:#}");
                failure = Some(e);

                RuntimeError(message)
            })
        });

        if let Some(e) = failure {
            return Err(e);
        }

        scoped.map_err(|e| anyhow!("{e}"))
    }
}
