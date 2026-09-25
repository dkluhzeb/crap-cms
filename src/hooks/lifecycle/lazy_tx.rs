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

use std::{cell::OnceCell, marker::PhantomData, ptr};

use anyhow::{Context as _, Result};
use mlua::Lua;
use tracing::error;

use crate::{
    db::{BoxedConnection, DbConnection, DbPool},
    hooks::lifecycle::types::{TxContext, restore_slot},
};

/// Commit `conn`'s open transaction, rolling it back when the commit fails
/// so the connection never returns to its pool mid-transaction.
///
/// # Errors
///
/// Returns the commit error.
pub(crate) fn commit_or_roll_back(conn: &dyn DbConnection, label: &str) -> Result<()> {
    let Err(e) = conn.execute("COMMIT", &[]) else {
        return Ok(());
    };

    roll_back(conn, label);

    Err(e.context(format!("failed to commit the {label} transaction")))
}

/// Roll back `conn`'s open transaction; a failure is logged (there is
/// nothing left to undo it with).
pub(crate) fn roll_back(conn: &dyn DbConnection, label: &str) {
    let _ = conn
        .execute("ROLLBACK", &[])
        .inspect_err(|e| error!("{label} rollback failed: {e:#}"));
}

/// A write transaction a hook opens lazily (see the module docs). Dropping
/// an open one rolls it back.
pub(crate) struct LazyTx {
    pool: DbPool,
    label: &'static str,
    conn: OnceCell<BoxedConnection>,
}

impl LazyTx {
    /// A not-yet-opened transaction on `pool`'s write side; `label` names
    /// the hook in error messages (e.g. "auth-callback").
    pub(crate) fn new(pool: DbPool, label: &'static str) -> Self {
        Self {
            pool,
            label,
            conn: OnceCell::new(),
        }
    }

    /// The transaction's connection, opening it (a write-pool connection
    /// plus `BEGIN`) on first use.
    ///
    /// # Errors
    ///
    /// Returns an error when no write connection can be acquired or the
    /// transaction cannot be opened.
    pub(crate) fn conn(&self) -> Result<&BoxedConnection> {
        if let Some(conn) = self.conn.get() {
            return Ok(conn);
        }

        let label = self.label;
        let conn = self
            .pool
            .write()
            .with_context(|| format!("{label} transaction: no write connection"))?;

        conn.execute("BEGIN", &[])
            .with_context(|| format!("failed to open the {label} transaction"))?;

        Ok(self.conn.get_or_init(|| conn))
    }

    /// Whether the hook opened the transaction (made a CRUD call).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_open(&self) -> bool {
        self.conn.get().is_some()
    }

    /// Commit the transaction, if the hook opened one.
    ///
    /// # Errors
    ///
    /// Returns the commit error (the transaction is rolled back).
    pub(crate) fn commit(mut self) -> Result<()> {
        let Some(conn) = self.conn.take() else {
            return Ok(());
        };

        commit_or_roll_back(&conn, self.label)
    }
}

impl Drop for LazyTx {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            roll_back(&conn, self.label);
        }
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
    pub(crate) unsafe fn tx<'a>(self) -> &'a LazyTx {
        unsafe { &*ptr::with_exposed_provenance::<LazyTx>(self.0) }
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
    _tx: PhantomData<&'a LazyTx>,
}

impl<'a> LazyTxGuard<'a> {
    /// Install `tx` on `lua` until the guard drops.
    #[must_use]
    pub(crate) fn install(lua: &'a Lua, tx: &'a LazyTx) -> Self {
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
        let tx = LazyTx::new(pool, "test");

        assert!(!tx.is_open());
        tx.commit()
            .expect("committing an unopened transaction is a no-op");
    }

    /// A committed transaction's writes persist; a dropped one's roll back.
    #[test]
    fn commit_persists_and_drop_rolls_back() {
        let (_dir, pool) = with_table();

        let tx = LazyTx::new(pool.clone(), "test");
        tx.conn()
            .unwrap()
            .execute("INSERT INTO t VALUES (1)", &[])
            .unwrap();
        assert!(tx.is_open());
        tx.commit().unwrap();
        assert_eq!(count(&pool), 1);

        let tx = LazyTx::new(pool.clone(), "test");
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
        let tx = LazyTx::new(pool, "test");

        {
            let _guard = LazyTxGuard::install(&lua, &tx);
            assert!(lua.app_data_ref::<LazyTxContext>().is_some());

            let conn = tx.conn().unwrap();
            lua.set_app_data(TxContext::new(conn));
        }

        assert!(lua.app_data_ref::<LazyTxContext>().is_none());
        assert!(lua.app_data_ref::<TxContext>().is_none());
    }
}
