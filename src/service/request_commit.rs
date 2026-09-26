//! Admitting a commit against the request it serves.
//!
//! A write made for an admin request with a deadline may commit only while
//! the request's commit gate admits it (see [`crate::core::commit_gate`]); a
//! write for no such request — a job, the CLI, gRPC — always may. Every write
//! chokepoint of a request asks here right before its commit point: `COMMIT`
//! for a transaction, the statement itself for a single autocommit write.

use anyhow::Context as _;

use crate::{core::admit_request_commit, db::BoxedTransaction, service::ServiceError};

/// Admit the commit about to happen on this thread.
///
/// # Errors
///
/// [`ServiceError::Transient`] when the request's deadline passed with
/// nothing committed: the caller must not commit — rolling its transaction
/// back, or skipping its autocommit write.
pub(crate) fn admit_commit() -> Result<(), ServiceError> {
    admit_request_commit().map_err(|e| ServiceError::Transient(e.into()))
}

/// Commit `tx` once [`admit_commit`] admits it; a refused one is rolled back.
///
/// # Errors
///
/// The refusal (see [`admit_commit`]), or the commit's backend error.
pub(crate) fn commit_admitted(tx: BoxedTransaction<'_>) -> Result<(), ServiceError> {
    admit_commit()?;

    tx.commit().context("Commit transaction")?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{CommitGate, in_commit_gate},
        db::{DbConnection, DbPool, pool},
    };

    fn pool_with_table() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let pool = pool::create_pool(dir.path(), &config).expect("pool");

        pool.write()
            .unwrap()
            .execute_batch("CREATE TABLE t (x INTEGER)")
            .unwrap();

        (dir, pool)
    }

    fn insert_and_commit(pool: &DbPool) -> Result<(), ServiceError> {
        let mut conn = pool.write().unwrap();
        let tx = conn.transaction_immediate().unwrap();
        tx.execute("INSERT INTO t (x) VALUES (1)", &[]).unwrap();

        commit_admitted(tx)
    }

    fn rows(pool: &DbPool) -> i64 {
        pool.get()
            .unwrap()
            .query_one("SELECT COUNT(*) AS c FROM t", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap()
    }

    /// Without a request gate, and within one that is open, the commit lands.
    #[test]
    fn an_admitted_commit_lands() {
        let (_dir, pool) = pool_with_table();

        insert_and_commit(&pool).expect("no gate");

        let open = CommitGate::new(Instant::now() + Duration::from_mins(1));
        in_commit_gate(Some(open), || insert_and_commit(&pool)).expect("open gate");

        assert_eq!(rows(&pool), 2);
    }

    /// A commit reaching a gate whose deadline passed is refused and rolled
    /// back: nothing is written.
    #[test]
    fn a_refused_commit_writes_nothing() {
        let (_dir, pool) = pool_with_table();

        let late = CommitGate::new(Instant::now());
        let refused = in_commit_gate(Some(late.clone()), || insert_and_commit(&pool));

        assert!(matches!(refused, Err(ServiceError::Transient(_))));
        assert!(late.expired());
        assert_eq!(rows(&pool), 0);
    }
}
