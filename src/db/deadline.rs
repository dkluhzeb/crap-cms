//! Statement deadlines: a runaway query is interrupted instead of holding its
//! connection — and on `SQLite` the single writer — indefinitely.
//!
//! Every statement runs under a budget: the configured `[database]
//! statement_timeout`, cut shorter by the deadline of the operation around
//! it ([`StatementDeadlineScope`], set from an operation's `OpDeadline`).
//! Long-running maintenance — the schema sync, a backup — lifts it for its
//! duration ([`UnboundedStatements`]).
//!
//! The budget is tracked per thread: a statement executes synchronously on
//! the calling thread on both backends (Postgres through `block_in_place`),
//! so the scopes a caller opens apply to exactly the statements it runs.
//! `SQLite` checks it from a progress handler installed on every pooled
//! connection; Postgres bounds the statement's future and cancels it on the
//! server when the budget runs out. Either way the statement fails with a
//! [`StatementTimedOut`] error.

use std::{
    cell::Cell,
    error::Error,
    fmt,
    time::{Duration, Instant},
};

thread_local! {
    /// When the statement running on this thread started.
    static STATEMENT_START: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The deadline of the operation this thread's statements run for.
    static SCOPE_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// How many [`UnboundedStatements`] scopes are open on this thread.
    static UNBOUNDED: Cell<u32> = const { Cell::new(0) };
    /// The running statement was interrupted for running out of time.
    static INTERRUPTED: Cell<bool> = const { Cell::new(false) };
}

/// The configured `[database] statement_timeout` of `secs` seconds — `None`
/// for `0`, which turns it off.
pub(crate) fn configured_timeout(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// A statement ran past its budget and was interrupted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementTimedOut;

impl fmt::Display for StatementTimedOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "the database statement ran past its time limit and was interrupted \
             (`[database] statement_timeout`, or the deadline of the operation it ran for)",
        )
    }
}

impl Error for StatementTimedOut {}

/// Marks one statement as running on this thread for as long as it lives.
pub(crate) struct StatementRun {
    prev: Option<Instant>,
}

impl StatementRun {
    /// A statement starts now.
    pub(crate) fn start() -> Self {
        INTERRUPTED.with(|i| i.set(false));

        Self {
            prev: STATEMENT_START.with(|s| s.replace(Some(Instant::now()))),
        }
    }

    /// Whether the statement running on this thread — the one the live
    /// [`StatementRun`] marks — was interrupted for running out of time.
    pub(crate) fn timed_out() -> bool {
        INTERRUPTED.with(Cell::get)
    }
}

impl Drop for StatementRun {
    fn drop(&mut self) {
        STATEMENT_START.with(|s| s.set(self.prev));
    }
}

/// The time a statement starting now may run, with `timeout` configured
/// (`None`: no statement timeout): the shorter of `timeout` and what is left
/// of the operation's deadline. `None` when nothing bounds it — neither is
/// set, or an [`UnboundedStatements`] scope is open.
#[cfg(any(feature = "postgres", test))]
pub(crate) fn statement_budget(timeout: Option<Duration>) -> Option<Duration> {
    if UNBOUNDED.with(Cell::get) > 0 {
        return None;
    }

    let scope = SCOPE_DEADLINE
        .with(Cell::get)
        .map(|deadline| deadline.saturating_duration_since(Instant::now()));

    match (timeout, scope) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Whether the statement running on this thread is past its budget — the
/// `SQLite` progress handler's verdict (`true` interrupts it). Records the
/// interruption for [`StatementRun::timed_out`].
pub(crate) fn statement_overdue(timeout: Option<Duration>) -> bool {
    let Some(start) = STATEMENT_START.with(Cell::get) else {
        return false;
    };

    if UNBOUNDED.with(Cell::get) > 0 {
        return false;
    }

    let by_timeout = timeout.is_some_and(|t| start.elapsed() >= t);
    let by_scope = SCOPE_DEADLINE
        .with(Cell::get)
        .is_some_and(|deadline| Instant::now() >= deadline);

    let overdue = by_timeout || by_scope;

    if overdue {
        INTERRUPTED.with(|i| i.set(true));
    }

    overdue
}

/// Bounds every statement this thread runs, while it lives, by an
/// operation's deadline (`None`: the operation has none). Scopes nest; the
/// earlier deadline wins.
pub struct StatementDeadlineScope {
    prev: Option<Instant>,
}

impl StatementDeadlineScope {
    /// Bound this thread's statements by `deadline` until the scope drops.
    #[must_use]
    pub fn bound_to(deadline: Option<Instant>) -> Self {
        let prev = SCOPE_DEADLINE.with(Cell::get);
        let next = match (prev, deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };

        SCOPE_DEADLINE.with(|s| s.set(next));

        Self { prev }
    }
}

impl Drop for StatementDeadlineScope {
    fn drop(&mut self) {
        SCOPE_DEADLINE.with(|s| s.set(self.prev));
    }
}

/// Lifts the statement budget on this thread while it lives — for
/// maintenance whose statements legitimately run long (the schema sync and
/// its migrations, a backup).
pub struct UnboundedStatements(());

impl UnboundedStatements {
    /// Lift the budget until the scope drops.
    #[must_use]
    pub fn lift() -> Self {
        UNBOUNDED.with(|u| u.set(u.get() + 1));

        Self(())
    }
}

impl Drop for UnboundedStatements {
    fn drop(&mut self) {
        UNBOUNDED.with(|u| u.set(u.get().saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Option<Duration> = Some(Duration::from_secs(1));

    #[test]
    fn the_budget_is_the_timeout_unless_the_operation_ends_sooner() {
        assert_eq!(statement_budget(None), None);
        assert_eq!(statement_budget(SECOND), SECOND);

        let _scope = StatementDeadlineScope::bound_to(Some(Instant::now()));
        let budget = statement_budget(SECOND).expect("bounded");
        assert!(budget < Duration::from_millis(10), "{budget:?}");
        assert!(
            statement_budget(None).is_some(),
            "the scope alone bounds it"
        );
    }

    #[test]
    fn lifting_removes_every_bound_until_the_scope_ends() {
        let _deadline = StatementDeadlineScope::bound_to(Some(Instant::now()));

        {
            let _lift = UnboundedStatements::lift();
            assert_eq!(statement_budget(SECOND), None);

            let _run = StatementRun::start();
            assert!(!statement_overdue(Some(Duration::ZERO)));
            assert!(!StatementRun::timed_out());
        }

        assert!(statement_budget(SECOND).is_some());
    }

    #[test]
    fn an_overdue_statement_is_recorded_as_timed_out() {
        let run = StatementRun::start();
        assert!(!statement_overdue(SECOND));
        assert!(!StatementRun::timed_out());

        assert!(statement_overdue(Some(Duration::ZERO)));
        assert!(StatementRun::timed_out());

        drop(run);
        assert!(
            !statement_overdue(Some(Duration::ZERO)),
            "no statement is running"
        );
    }

    #[test]
    fn scopes_nest_and_restore() {
        let later = Instant::now() + Duration::from_mins(1);
        let sooner = Instant::now() + Duration::from_secs(1);

        let _outer = StatementDeadlineScope::bound_to(Some(sooner));
        {
            let _inner = StatementDeadlineScope::bound_to(Some(later));
            assert!(statement_budget(None).expect("bounded") <= Duration::from_secs(1));
        }

        assert!(statement_budget(None).expect("bounded") <= Duration::from_secs(1));
    }
}
