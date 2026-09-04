//! Cooperative wall-clock deadline for long batch operations.
//!
//! A queued bulk run executes inside `spawn_blocking`, which Tokio cannot
//! cancel: when the scheduler's job timeout fires it can only *record* a
//! failure while the batch keeps running and possibly commits — leaving a
//! committed batch recorded as failed.
//!
//! [`OpDeadline`] makes the timeout **enforced** instead of merely
//! reported. The bulk loops check it between documents; on expiry the
//! operation returns an error, its transaction rolls back (bulk ops are
//! atomic), and nothing is committed — so the recorded failure is true.
//! The scheduler sets this deadline to the job's configured budget and
//! gives its own uncancellable outer timer extra grace on top, so the
//! cooperative abort always wins the race.
//!
//! Synchronous callers pass [`OpDeadline::none`] and are unaffected.

use std::time::Instant;

use crate::service::ServiceError;

/// An optional instant after which a batch operation must abort.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpDeadline(Option<Instant>);

impl OpDeadline {
    /// No deadline — the operation runs to completion (every synchronous
    /// surface).
    #[must_use]
    pub fn none() -> Self {
        Self(None)
    }

    /// Abort once `instant` has passed.
    #[must_use]
    pub fn at(instant: Instant) -> Self {
        Self(Some(instant))
    }

    /// Abort `after` from now. On the (unreachable) `Instant` overflow the
    /// deadline is treated as ALREADY passed — failing closed, since
    /// "never abort" is the unsafe direction here.
    #[must_use]
    pub fn in_secs(after: u64) -> Self {
        let now = Instant::now();

        Self(Some(
            now.checked_add(std::time::Duration::from_secs(after))
                .unwrap_or(now),
        ))
    }

    /// Whether the deadline has passed.
    #[must_use]
    pub fn expired(self) -> bool {
        self.0.is_some_and(|d| Instant::now() >= d)
    }

    /// The check the batch loops call between documents. `processed` is the
    /// count completed so far, reported in the error so an operator can see
    /// how far the batch got before it was abandoned (all of it rolled back).
    ///
    /// # Errors
    ///
    /// [`ServiceError::LimitExceeded`] once the deadline has passed — the
    /// same class as the `bulk_max_documents` cap (an operation exceeded a
    /// configured limit and changed nothing), and, unlike `Transient`, a
    /// user-facing message that survives the job runner's error
    /// sanitization so the operator sees how far the batch got.
    pub fn check(self, processed: i64) -> Result<(), ServiceError> {
        if !self.expired() {
            return Ok(());
        }

        Err(ServiceError::LimitExceeded(format!(
            "operation exceeded its time limit after {processed} document(s); \
             the whole batch was rolled back — nothing was committed. Raise \
             the queue's `timeout` (or narrow the batch) and re-run."
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Regression: an overflowing duration must not silently become "no
    /// deadline" — that would let a runaway batch run unbounded.
    #[test]
    fn overflow_fails_closed() {
        assert!(OpDeadline::in_secs(u64::MAX).expired());
        assert!(OpDeadline::in_secs(u64::MAX).check(0).is_err());
    }

    #[test]
    fn none_never_expires() {
        let d = OpDeadline::none();
        assert!(!d.expired());
        assert!(d.check(0).is_ok());
    }

    #[test]
    fn future_deadline_passes_past_deadline_fails() {
        assert!(OpDeadline::in_secs(60).check(3).is_ok());

        let past = OpDeadline::at(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .expect("clock is past the epoch"),
        );
        let err = past.check(7).unwrap_err();

        assert!(matches!(err, ServiceError::LimitExceeded(_)));
        let msg = err.to_string();
        assert!(msg.contains("7 document(s)"), "{msg}");
        assert!(msg.contains("rolled back"), "{msg}");
    }
}
