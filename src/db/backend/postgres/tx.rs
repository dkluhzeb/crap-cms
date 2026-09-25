//! Transaction bookkeeping for the Postgres backend: what the server does not
//! report back through the driver.
//!
//! Postgres aborts a transaction at its first failed statement, and answers
//! the `COMMIT` of an aborted transaction with a `ROLLBACK` — no error.
//! `tokio-postgres` reads that answer as success, so a caller that caught a
//! failed statement's error and carried on would be told its whole
//! transaction committed while every write in it was discarded. Each client
//! therefore records whether a statement failed since its transaction began,
//! and a commit after one first asks the server whether the transaction is
//! still usable.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Error, Result, anyhow};
use tokio_postgres::{GenericClient, error::SqlState};

/// The message of a commit refused because the transaction was aborted.
const ABORTED: &str = "the transaction was rolled back, not committed: a statement inside it \
                       failed, and Postgres aborts the whole transaction at the first failed \
                       statement";

/// Per-client transaction state, kept beside the statement cache.
#[derive(Default)]
pub(crate) struct TxState {
    /// A transaction opened in place (`BEGIN` on the pooled client) is open.
    in_place: AtomicBool,
    /// A statement failed since the current transaction began.
    failed: AtomicBool,
}

impl TxState {
    /// A transaction begins: nothing inside it has failed yet.
    pub(super) fn began(&self, in_place: bool) {
        self.failed.store(false, Ordering::Relaxed);
        self.in_place.store(in_place, Ordering::Relaxed);
    }

    /// The in-place transaction was settled (committed or rolled back).
    pub(super) fn settled(&self) {
        self.in_place.store(false, Ordering::Relaxed);
    }

    /// Whether an in-place transaction is open on the client.
    pub(super) fn in_place(&self) -> bool {
        self.in_place.load(Ordering::Relaxed)
    }

    /// Note the outcome of one statement.
    pub(super) fn record<T>(&self, result: &Result<T>) {
        if result.is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
    }
}

/// Refuse to commit a transaction the server already aborted.
///
/// Free when nothing failed. After a failure the transaction may still be
/// usable — the failed statement ran inside a savepoint that was rolled back
/// to — so the server is asked with a trivial statement, which an aborted
/// transaction refuses with `25P02`.
///
/// # Errors
///
/// Returns an error when the transaction is aborted, or when the probe
/// itself fails for another reason.
pub(super) async fn ensure_not_aborted<C: GenericClient>(
    client: &C,
    state: &TxState,
) -> Result<()> {
    if !state.failed.load(Ordering::Relaxed) {
        return Ok(());
    }

    let Err(e) = client.batch_execute("SELECT 1").await else {
        return Ok(());
    };

    if e.code() == Some(&SqlState::IN_FAILED_SQL_TRANSACTION) {
        return Err(anyhow!(ABORTED));
    }

    Err(Error::new(e).context("failed to check the transaction before committing it"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_statement_is_remembered_until_the_next_transaction_begins() {
        let state = TxState::default();
        state.began(true);
        assert!(state.in_place());

        state.record::<()>(&Err(anyhow!("boom")));
        assert!(state.failed.load(Ordering::Relaxed));

        state.record(&Ok(()));
        assert!(
            state.failed.load(Ordering::Relaxed),
            "a later success does not clear the failure"
        );

        state.settled();
        assert!(!state.in_place());

        state.began(false);
        assert!(!state.failed.load(Ordering::Relaxed));
    }
}
