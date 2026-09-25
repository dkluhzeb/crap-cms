//! A hook transaction opened at the hook's first CRUD call, not up front.
//!
//! Auth-callback hooks and `mfa_deliver` hooks spend most of their time on
//! outbound HTTP (an OAuth token exchange, an SMS gateway) and touch the
//! database, if at all, around it. Taking a write-pool connection for the
//! whole call would pin one of the few write connections — on `SQLite`, the
//! only writer — across that network I/O. [`LazyTx`] defers it: the hook's
//! first `crap.*` CRUD call acquires the write connection and opens the
//! transaction, every later call shares it, and the runner commits it
//! ([`LazyTx::commit`]) or rolls it back (dropping the [`LazyTx`]) when the
//! hook returns. A hook that does no CRUD never touches the write pool.
//!
//! An auth strategy runs the same way, with the caller's connection as its
//! reader: its reads before any write run on that connection — the request's
//! read connection, for a per-request strategy — and only a write takes a
//! write connection.

use std::{cell::OnceCell, marker::PhantomData, ptr};

use anyhow::{Context as _, Result};
use mlua::Lua;

use crate::{
    db::{DbConnection, DbPool, InPlaceTransaction},
    hooks::lifecycle::types::{TxContext, restore_slot},
};

/// A write transaction a hook opens lazily (see the module docs), on a
/// write-pool connection: IMMEDIATE on `SQLite`, like every other write
/// scope, so the hook's usual read-then-write (look the user up, then
/// provision it) cannot fail with `SQLITE_BUSY_SNAPSHOT`. Dropping an open
/// one rolls it back.
///
/// Given a reader — the caller's own connection — the hook's reads before
/// its first write run there, in autocommit, and only a write opens the
/// transaction: a per-request auth strategy that only looks its user up
/// never takes a write connection.
pub(crate) struct LazyTx<'c> {
    pool: DbPool,
    reader: Option<&'c dyn DbConnection>,
    label: &'static str,
    tx: OnceCell<InPlaceTransaction<'static>>,
}

impl LazyTx<'static> {
    /// A not-yet-opened transaction on `pool`'s write side; `label` names
    /// the hook in error messages (e.g. "auth-callback").
    pub(crate) fn on_pool(pool: DbPool, label: &'static str) -> Self {
        Self::new(pool, None, label)
    }
}

impl<'c> LazyTx<'c> {
    /// A not-yet-opened transaction on `pool`'s write side, whose reads run
    /// on `reader` until the first write opens it.
    pub(crate) fn on_pool_reading(
        pool: DbPool,
        reader: &'c dyn DbConnection,
        label: &'static str,
    ) -> Self {
        Self::new(pool, Some(reader), label)
    }

    fn new(pool: DbPool, reader: Option<&'c dyn DbConnection>, label: &'static str) -> Self {
        Self {
            pool,
            reader,
            label,
            tx: OnceCell::new(),
        }
    }

    /// The hook this transaction belongs to, for error messages.
    pub(crate) fn label(&self) -> &'static str {
        self.label
    }

    /// The connection a read runs on while no write opened the transaction:
    /// the reader, if one was given. `None` once the transaction is open —
    /// every call then shares it.
    pub(crate) fn reader(&self) -> Option<&'c dyn DbConnection> {
        self.reader.filter(|_| self.tx.get().is_none())
    }

    /// The transaction's connection, opening the transaction on first use.
    ///
    /// # Errors
    ///
    /// Returns an error when no write connection can be acquired or the
    /// transaction cannot be opened.
    pub(crate) fn conn(&self) -> Result<&dyn DbConnection> {
        if let Some(tx) = self.tx.get() {
            return Ok(tx.conn());
        }

        let tx = self
            .open()
            .with_context(|| format!("failed to open the {} transaction", self.label))?;

        Ok(self.tx.get_or_init(|| tx).conn())
    }

    fn open(&self) -> Result<InPlaceTransaction<'static>> {
        let conn = self.pool.write().context("no write connection")?;

        InPlaceTransaction::begin_owned(conn)
    }

    /// Whether the hook opened the transaction (made a CRUD call).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_open(&self) -> bool {
        self.tx.get().is_some()
    }

    /// Commit the transaction, if the hook opened one.
    ///
    /// # Errors
    ///
    /// Returns the commit error (the transaction is rolled back).
    pub(crate) fn commit(mut self) -> Result<()> {
        let Some(tx) = self.tx.take() else {
            return Ok(());
        };

        tx.commit()
            .with_context(|| format!("failed to commit the {} transaction", self.label))
    }
}

/// App-data handle to the [`LazyTx`] a [`LazyTxGuard`] installed: its
/// address, kept as a plain word so the slot is `'static` (the same erasure
/// [`TxContext`] uses for its connection pointer).
#[derive(Clone, Copy)]
pub(crate) struct LazyTxContext(usize);

impl LazyTxContext {
    /// The installed transaction.
    ///
    /// # Safety
    ///
    /// Only while the [`LazyTxGuard`] that installed this context is alive —
    /// the guard borrows the [`LazyTx`], so it cannot move or drop meanwhile,
    /// and removes this context when it drops.
    pub(crate) unsafe fn tx<'a>(self) -> &'a LazyTx<'a> {
        unsafe { &*ptr::with_exposed_provenance::<LazyTx<'a>>(self.0) }
    }
}

/// Installs a [`LazyTx`] as `lua`'s pending hook transaction. On drop it
/// restores the previous context — including removing the [`TxContext`] the
/// transaction's first CRUD call installed — so nothing on the VM points at
/// the transaction once the guard is gone. Borrowing the [`LazyTx`] for the
/// guard's lifetime makes "the transaction outlives its context" a compile
/// error to get wrong.
pub(crate) struct LazyTxGuard<'a> {
    lua: &'a Lua,
    prev_lazy: Option<LazyTxContext>,
    prev_tx: Option<TxContext>,
    _tx: PhantomData<&'a LazyTx<'a>>,
}

impl<'a> LazyTxGuard<'a> {
    /// Install `tx` on `lua` until the guard drops.
    #[must_use]
    pub(crate) fn install(lua: &'a Lua, tx: &'a LazyTx<'_>) -> Self {
        let guard = Self {
            lua,
            prev_lazy: lua.app_data_ref::<LazyTxContext>().map(|r| *r),
            prev_tx: lua.app_data_ref::<TxContext>().map(|r| *r),
            _tx: PhantomData,
        };

        lua.remove_app_data::<TxContext>();
        lua.set_app_data(LazyTxContext(ptr::from_ref(tx).expose_provenance()));

        guard
    }
}

impl Drop for LazyTxGuard<'_> {
    fn drop(&mut self) {
        restore_slot(self.lua, self.prev_tx.take());
        restore_slot(self.lua, self.prev_lazy.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{config::CrapConfig, db::pool};

    fn test_pool() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let pool = pool::create_pool(dir.path(), &config).expect("pool");

        (dir, pool)
    }

    fn count(pool: &DbPool) -> i64 {
        pool.get()
            .unwrap()
            .query_one("SELECT COUNT(*) AS c FROM t", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap()
    }

    fn with_table() -> (tempfile::TempDir, DbPool) {
        let (dir, pool) = test_pool();
        pool.write()
            .unwrap()
            .execute_batch("CREATE TABLE t (x INTEGER)")
            .unwrap();

        (dir, pool)
    }

    /// Nothing is acquired until the first use.
    #[test]
    fn an_unused_transaction_never_opens() {
        let (_dir, pool) = test_pool();
        let tx = LazyTx::on_pool(pool, "test");

        assert!(!tx.is_open());
        tx.commit()
            .expect("committing an unopened transaction is a no-op");
    }

    /// A committed transaction's writes persist; a dropped one's roll back.
    #[test]
    fn commit_persists_and_drop_rolls_back() {
        let (_dir, pool) = with_table();

        let tx = LazyTx::on_pool(pool.clone(), "test");
        tx.conn()
            .unwrap()
            .execute("INSERT INTO t VALUES (1)", &[])
            .unwrap();
        assert!(tx.is_open());
        tx.commit().unwrap();
        assert_eq!(count(&pool), 1);

        let tx = LazyTx::on_pool(pool.clone(), "test");
        tx.conn()
            .unwrap()
            .execute("INSERT INTO t VALUES (2)", &[])
            .unwrap();
        drop(tx);
        assert_eq!(count(&pool), 1, "a dropped transaction rolls back");
    }

    /// The guard's context is removed on drop — along with the connection
    /// context the first CRUD call installs.
    #[test]
    fn the_guard_removes_both_contexts() {
        let (_dir, pool) = test_pool();
        let lua = Lua::new();
        let tx = LazyTx::on_pool(pool, "test");

        {
            let _guard = LazyTxGuard::install(&lua, &tx);
            assert!(lua.app_data_ref::<LazyTxContext>().is_some());

            let conn = tx.conn().unwrap();
            lua.set_app_data(TxContext::new(conn));
        }

        assert!(lua.app_data_ref::<LazyTxContext>().is_none());
        assert!(lua.app_data_ref::<TxContext>().is_none());
    }

    /// Regression: the lazy transaction opened with a deferred `BEGIN`, so a
    /// hook's read-then-write could fail with `SQLITE_BUSY_SNAPSHOT` when
    /// another connection committed in between. It now takes the write lock
    /// when it opens — another writer waits for it instead.
    #[test]
    fn the_transaction_takes_the_write_lock_when_it_opens() {
        let (_dir, pool) = with_table();
        let tx = LazyTx::on_pool(pool.clone(), "test");

        tx.conn()
            .unwrap()
            .query_one("SELECT COUNT(*) FROM t", &[])
            .unwrap();

        let other = pool.get().unwrap();
        other.execute_batch("PRAGMA busy_timeout = 50").unwrap();
        assert!(
            other.execute("INSERT INTO t VALUES (9)", &[]).is_err(),
            "a read-only start must still hold the write lock"
        );

        drop(tx);
    }

    /// Reads run on the reader until a write opens the transaction — then
    /// every call shares it.
    #[test]
    fn reads_run_on_the_reader_until_a_write_opens_the_transaction() {
        let (_dir, pool) = with_table();
        let reader = pool.get().unwrap();

        let tx = LazyTx::on_pool_reading(pool.clone(), &reader, "test");
        assert!(tx.reader().is_some());
        assert!(!tx.is_open());

        tx.conn()
            .unwrap()
            .execute("INSERT INTO t VALUES (1)", &[])
            .unwrap();
        assert!(
            tx.reader().is_none(),
            "the open transaction serves reads too"
        );

        tx.commit().unwrap();
        assert_eq!(count(&pool), 1);
    }
}
