//! The commit gate of a request that has a deadline.
//!
//! A request answered `408 Request Timeout` must have changed nothing. Its
//! write runs on a blocking thread Tokio cannot cancel, so racing the handler
//! future alone would answer `408` and let the write commit a moment later —
//! the client, told nothing happened, re-submits and creates a duplicate.
//!
//! The gate settles that race once, atomically. The admin middleware
//! *expires* it when the deadline passes; every write chokepoint asks it to
//! *admit* its commit right before `COMMIT`. Whichever comes first wins: an
//! expired gate refuses every later commit (the write rolls back), and a gate
//! that admitted a commit tells the middleware the outcome is no longer
//! "nothing happened" — it waits for the handler's own answer instead.
//!
//! The middleware enters the gate for the request's task
//! ([`with_commit_gate`]); work moved onto a blocking thread carries it there
//! ([`in_commit_gate`], which
//! [`spawn_request_blocking`](crate::core::spawn_request_blocking) applies),
//! and the write chokepoints read it from the thread
//! ([`admit_request_commit`], [`request_deadline`]). A thread without a
//! gate — a job, the CLI, gRPC — commits freely.

use std::{
    cell::RefCell,
    error::Error,
    fmt,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

use tokio::task_local;

/// No commit admitted, deadline not yet declared passed.
const OPEN: u8 = 0;
/// A commit was admitted: the request's outcome is the handler's to report.
const COMMITTING: u8 = 1;
/// The deadline passed with nothing committed: nothing ever will be.
const EXPIRED: u8 = 2;

task_local! {
    /// The gate of the request in progress on this task.
    static REQUEST_GATE: Arc<CommitGate>;
}

thread_local! {
    /// The gate of the request this thread is working for.
    static THREAD_GATE: RefCell<Option<Arc<CommitGate>>> = const { RefCell::new(None) };
}

/// A write refused because its request's deadline passed before it could
/// commit. The transaction is rolled back; nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestDeadlinePassed;

impl fmt::Display for RequestDeadlinePassed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the request's deadline passed before its write committed; nothing was changed")
    }
}

impl Error for RequestDeadlinePassed {}

/// One request's commit gate (see the module docs).
#[derive(Debug)]
pub struct CommitGate {
    deadline: Instant,
    state: AtomicU8,
}

impl CommitGate {
    /// A gate for a request that must be answered by `deadline`.
    #[must_use]
    pub fn new(deadline: Instant) -> Arc<Self> {
        Arc::new(Self {
            deadline,
            state: AtomicU8::new(OPEN),
        })
    }

    /// When the request's time is up.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Declare the deadline passed. `true` when no commit was admitted — none
    /// ever will be now, so the request may be answered as timed out; `false`
    /// when one already was, and the outcome is the handler's to report.
    #[must_use]
    pub fn expire(&self) -> bool {
        let outcome =
            self.state
                .compare_exchange(OPEN, EXPIRED, Ordering::AcqRel, Ordering::Acquire);

        match outcome {
            Ok(_) => true,
            Err(current) => current == EXPIRED,
        }
    }

    /// Whether the gate expired: the request's deadline passed with nothing
    /// committed.
    #[must_use]
    pub fn expired(&self) -> bool {
        self.state.load(Ordering::Acquire) == EXPIRED
    }

    /// Admit a commit — called right before `COMMIT`. Once a commit was
    /// admitted, later ones are too (the request already changed something).
    ///
    /// # Errors
    ///
    /// [`RequestDeadlinePassed`] when the deadline passed with nothing
    /// committed; the gate is then expired for good.
    pub fn admit_commit(&self) -> Result<(), RequestDeadlinePassed> {
        let next = if Instant::now() >= self.deadline {
            EXPIRED
        } else {
            COMMITTING
        };

        let outcome = self
            .state
            .compare_exchange(OPEN, next, Ordering::AcqRel, Ordering::Acquire);

        let admitted = match outcome {
            Ok(_) => next == COMMITTING,
            Err(current) => current == COMMITTING,
        };

        admitted.then_some(()).ok_or(RequestDeadlinePassed)
    }
}

/// Run `fut` — a request's handling — under `gate`.
pub async fn with_commit_gate<F: Future>(gate: Arc<CommitGate>, fut: F) -> F::Output {
    REQUEST_GATE.scope(gate, fut).await
}

/// The gate of the request in progress on this task, if it has one. Capture
/// it before moving work onto a `spawn_blocking` thread and hand it to
/// [`in_commit_gate`] there.
#[must_use]
pub fn current_commit_gate() -> Option<Arc<CommitGate>> {
    REQUEST_GATE.try_with(Arc::clone).ok()
}

/// Restores the thread's previous gate on drop.
struct ThreadGateScope {
    prev: Option<Arc<CommitGate>>,
}

impl ThreadGateScope {
    fn enter(gate: Option<Arc<CommitGate>>) -> Self {
        let prev = THREAD_GATE.with(|slot| slot.replace(gate));

        Self { prev }
    }
}

impl Drop for ThreadGateScope {
    fn drop(&mut self) {
        let prev = self.prev.take();

        THREAD_GATE.with(|slot| *slot.borrow_mut() = prev);
    }
}

/// Run `f` synchronously with `gate` as this thread's commit gate (`None`: no
/// gate) — the `spawn_blocking` counterpart of [`with_commit_gate`].
pub fn in_commit_gate<R>(gate: Option<Arc<CommitGate>>, f: impl FnOnce() -> R) -> R {
    let _scope = ThreadGateScope::enter(gate);

    f()
}

/// Run `f` outside this thread's commit gate — for work that is not the
/// request's own write and runs once that write's outcome is settled (a
/// transaction's `on_commit` / `on_rollback` effects).
pub fn outside_commit_gate<R>(f: impl FnOnce() -> R) -> R {
    in_commit_gate(None, f)
}

/// The deadline of the request this thread works for, if it has one — what a
/// write bounds its statements by.
#[must_use]
pub fn request_deadline() -> Option<Instant> {
    THREAD_GATE.with(|slot| slot.borrow().as_ref().map(|gate| gate.deadline()))
}

/// Admit a commit on this thread's gate; a thread without one always commits.
/// Every pool-mode write calls it right before `COMMIT`.
///
/// # Errors
///
/// [`RequestDeadlinePassed`] when the request's deadline passed with nothing
/// committed.
pub fn admit_request_commit() -> Result<(), RequestDeadlinePassed> {
    let gate = THREAD_GATE.with(|slot| slot.borrow().clone());

    gate.map_or(Ok(()), |gate| gate.admit_commit())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::task::spawn_blocking;

    use super::*;

    fn open_gate() -> Arc<CommitGate> {
        CommitGate::new(Instant::now() + Duration::from_mins(1))
    }

    /// Whichever comes first wins: an expired gate refuses every commit, a
    /// gate that admitted one cannot be expired into a false "nothing
    /// happened".
    #[test]
    fn expiry_and_commit_exclude_each_other() {
        let gate = open_gate();
        assert!(gate.expire(), "nothing committed yet");
        assert!(gate.expired());
        assert_eq!(gate.admit_commit(), Err(RequestDeadlinePassed));

        let gate = open_gate();
        assert_eq!(gate.admit_commit(), Ok(()));
        assert!(!gate.expire(), "a commit was admitted");
        assert!(!gate.expired());
        assert_eq!(gate.admit_commit(), Ok(()), "later commits follow");
    }

    /// A write reaching its commit after the deadline is refused even before
    /// the middleware declared the deadline passed — and the gate then reads
    /// expired.
    #[test]
    fn a_commit_past_the_deadline_is_refused() {
        let gate = CommitGate::new(Instant::now());

        assert_eq!(gate.admit_commit(), Err(RequestDeadlinePassed));
        assert!(gate.expired());
        assert!(gate.expire());
    }

    /// The thread's gate applies inside its scope only; without one every
    /// commit is admitted.
    #[test]
    fn the_thread_gate_is_scoped() {
        let gate = CommitGate::new(Instant::now());

        assert_eq!(admit_request_commit(), Ok(()));
        assert_eq!(request_deadline(), None);

        in_commit_gate(Some(gate.clone()), || {
            assert_eq!(request_deadline(), Some(gate.deadline()));
            assert_eq!(admit_request_commit(), Err(RequestDeadlinePassed));

            outside_commit_gate(|| assert_eq!(admit_request_commit(), Ok(())));

            assert_eq!(admit_request_commit(), Err(RequestDeadlinePassed));
        });

        assert_eq!(admit_request_commit(), Ok(()));
    }

    /// The task's gate reaches work moved onto a blocking thread.
    #[tokio::test]
    async fn the_task_gate_is_captured_for_a_blocking_thread() {
        assert!(current_commit_gate().is_none());

        let gate = open_gate();
        let deadline = with_commit_gate(gate.clone(), async {
            let captured = current_commit_gate();

            spawn_blocking(move || in_commit_gate(captured, request_deadline))
                .await
                .unwrap()
        })
        .await;

        assert_eq!(deadline, Some(gate.deadline()));
    }
}
