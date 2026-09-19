//! What a failed statement or checkout *was*, read from the driver error's
//! type rather than from its text.
//!
//! Both backends report a constraint violation, a lost connection and a busy
//! database through a typed error carrying a machine-readable code — SQLSTATE
//! on Postgres, a result code on `SQLite`. The message beside it is not
//! machine-readable: Postgres translates it according to `lc_messages`, and the
//! two backends word the same condition differently ("UNIQUE constraint failed:
//! users.email" against "duplicate key value violates unique constraint"). So
//! every decision here is made on the code, in one place, and the callers keep
//! their message matching only as a fallback for errors that reach them with
//! the typed cause already erased (a hook re-wrapping a failure as a string, or
//! r2d2, whose pool error exposes no variant to match on).

#[cfg(feature = "postgres")]
use std::{error::Error as StdError, io::Error as IoError};

use anyhow::Error;

#[cfg(feature = "postgres")]
use deadpool::managed::PoolError;
#[cfg(feature = "sqlite")]
use rusqlite::{
    Error as SqliteError,
    Error::SqliteFailure,
    ErrorCode::{DatabaseBusy, DatabaseLocked},
    ffi::{SQLITE_CONSTRAINT_FOREIGNKEY, SQLITE_CONSTRAINT_PRIMARYKEY, SQLITE_CONSTRAINT_UNIQUE},
};
#[cfg(feature = "postgres")]
use tokio_postgres::{Error as PgError, error::SqlState};

/// The constraint a rejected write violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    /// A duplicate value in a column or index declared unique.
    Unique,
    /// A reference to a row that does not exist, or a delete of a row another
    /// table still references.
    ForeignKey,
}

/// The constraint the typed driver cause inside `e` reports as violated, or
/// `None` when the chain carries no driver error — or one that is not a
/// constraint failure at all.
#[must_use]
pub fn constraint_kind(e: &Error) -> Option<ConstraintKind> {
    sqlite_constraint_kind(e).or_else(|| pg_constraint_kind(e))
}

/// Whether the typed driver cause inside `e` is a condition the same request
/// could succeed at if repeated — a busy database, a lost or refused
/// connection, an exhausted pool, a server being restarted.
#[must_use]
pub fn is_transient(e: &Error) -> bool {
    sqlite_is_transient(e) || pg_is_transient(e)
}

// ── SQLite ───────────────────────────────────────────────────────────────

#[cfg(feature = "sqlite")]
fn sqlite_constraint_kind(e: &Error) -> Option<ConstraintKind> {
    let SqliteFailure(ffi, _) = e.downcast_ref::<SqliteError>()? else {
        return None;
    };

    // The extended result code, not the primary one: every constraint failure
    // shares `SQLITE_CONSTRAINT`, and only the extension says which constraint.
    match ffi.extended_code {
        SQLITE_CONSTRAINT_UNIQUE | SQLITE_CONSTRAINT_PRIMARYKEY => Some(ConstraintKind::Unique),
        SQLITE_CONSTRAINT_FOREIGNKEY => Some(ConstraintKind::ForeignKey),
        _ => None,
    }
}

#[cfg(not(feature = "sqlite"))]
fn sqlite_constraint_kind(_: &Error) -> Option<ConstraintKind> {
    None
}

#[cfg(feature = "sqlite")]
fn sqlite_is_transient(e: &Error) -> bool {
    e.downcast_ref::<SqliteError>()
        .and_then(SqliteError::sqlite_error_code)
        .is_some_and(|code| matches!(code, DatabaseBusy | DatabaseLocked))
}

#[cfg(not(feature = "sqlite"))]
fn sqlite_is_transient(_: &Error) -> bool {
    false
}

// ── Postgres ─────────────────────────────────────────────────────────────

#[cfg(feature = "postgres")]
fn pg_constraint_kind(e: &Error) -> Option<ConstraintKind> {
    let code = e.downcast_ref::<PgError>()?.code()?;

    if *code == SqlState::UNIQUE_VIOLATION {
        return Some(ConstraintKind::Unique);
    }

    if *code == SqlState::FOREIGN_KEY_VIOLATION {
        return Some(ConstraintKind::ForeignKey);
    }

    None
}

#[cfg(not(feature = "postgres"))]
fn pg_constraint_kind(_: &Error) -> Option<ConstraintKind> {
    None
}

#[cfg(feature = "postgres")]
fn pg_is_transient(e: &Error) -> bool {
    // A checkout that timed out waiting for a slot is the caller's cue to
    // retry; a checkout whose connect failed is judged like any other driver
    // error — a refused connection retries, a rejected password does not. A
    // closed pool is never transient: the process is shutting down.
    if let Some(pool_err) = e.downcast_ref::<PoolError<PgError>>() {
        return match pool_err {
            PoolError::Timeout(_) => true,
            PoolError::Backend(err) => pg_error_is_transient(err),
            _ => false,
        };
    }

    e.downcast_ref::<PgError>()
        .is_some_and(pg_error_is_transient)
}

#[cfg(feature = "postgres")]
fn pg_error_is_transient(err: &PgError) -> bool {
    // The driver already knows the connection is gone.
    if err.is_closed() {
        return true;
    }

    // A connect that never reached the server (refused, reset, timed out)
    // carries no SQLSTATE; the driver reports it as an I/O error.
    if err.code().is_none() {
        return StdError::source(err).is_some_and(<dyn StdError>::is::<IoError>);
    }

    // SQLSTATE classes: 08 connection exception, 53 insufficient resources (out
    // of connections, disk or memory), 57 operator intervention (admin
    // shutdown, cancelled query, crash-recovery restart). Each is a statement
    // the server refused for its own reasons, not a malformed request.
    err.code().is_some_and(|code| {
        let sqlstate = code.code();

        sqlstate.starts_with("08") || sqlstate.starts_with("53") || sqlstate.starts_with("57")
    })
}

#[cfg(not(feature = "postgres"))]
fn pg_is_transient(_: &Error) -> bool {
    false
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::{Context as _, anyhow};
    use rusqlite::ffi::{Error as FfiError, SQLITE_BUSY, SQLITE_LOCKED};

    use super::*;

    fn sqlite_failure(extended_code: i32, message: &str) -> Error {
        Error::new(SqliteFailure(
            FfiError::new(extended_code),
            Some(message.to_string()),
        ))
    }

    /// The kind comes from the extended result code, so the two constraint
    /// failures are told apart even though they share a primary code — and a
    /// foreign-key failure is no longer read as a unique violation.
    #[test]
    fn sqlite_constraint_kinds_come_from_the_extended_code() {
        assert_eq!(
            constraint_kind(&sqlite_failure(
                SQLITE_CONSTRAINT_UNIQUE,
                "UNIQUE constraint failed: users.email"
            )),
            Some(ConstraintKind::Unique)
        );
        assert_eq!(
            constraint_kind(&sqlite_failure(
                SQLITE_CONSTRAINT_PRIMARYKEY,
                "UNIQUE constraint failed: users.id"
            )),
            Some(ConstraintKind::Unique)
        );
        assert_eq!(
            constraint_kind(&sqlite_failure(
                SQLITE_CONSTRAINT_FOREIGNKEY,
                "FOREIGN KEY constraint failed"
            )),
            Some(ConstraintKind::ForeignKey)
        );
    }

    /// The typed cause is found however deep the `anyhow` context stack is —
    /// the query layer wraps every statement failure at least once.
    #[test]
    fn the_typed_cause_survives_context_layers() {
        let e = Err::<(), _>(sqlite_failure(
            SQLITE_CONSTRAINT_FOREIGNKEY,
            "FOREIGN KEY constraint failed",
        ))
        .context("Failed to create document in 'posts'")
        .context("create")
        .unwrap_err();

        assert_eq!(constraint_kind(&e), Some(ConstraintKind::ForeignKey));
    }

    #[test]
    fn a_busy_or_locked_database_is_transient_but_a_constraint_is_not() {
        assert!(is_transient(&sqlite_failure(
            SQLITE_BUSY,
            "database is locked"
        )));
        assert!(is_transient(&sqlite_failure(
            SQLITE_LOCKED,
            "database table is locked"
        )));
        assert!(!is_transient(&sqlite_failure(
            SQLITE_CONSTRAINT_UNIQUE,
            "UNIQUE constraint failed: users.email"
        )));
    }

    /// An error with no driver cause answers "don't know" rather than
    /// guessing, leaving the caller's message fallback to decide.
    #[test]
    fn a_plain_error_carries_no_verdict() {
        let e = anyhow!("UNIQUE constraint failed: users.email");

        assert_eq!(constraint_kind(&e), None);
        assert!(!is_transient(&e));
    }
}
