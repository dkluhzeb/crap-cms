//! Process lifecycle: readiness reporting and bounded graceful-shutdown drains.
//!
//! Both halves are shared across surfaces on purpose. A drain deadline that
//! only one server honours leaves the other able to hang the whole process
//! (its sibling never returns, so post-shutdown cleanup never runs), and a
//! readiness probe that only one surface knows about answers `200` while
//! startup recovery is still rewriting job rows.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use tokio::{select, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// How long a network server keeps draining in-flight connections after the
/// shutdown token fires before it is force-stopped. Long-lived streams (admin
/// SSE, gRPC `Subscribe`) end themselves on the same token, so this only
/// bounds the tail left by a client that stops reading.
pub const SERVER_DRAIN_SECS: u64 = 10;

/// Run `server` to completion, or abandon it once `deadline` has elapsed since
/// `shutdown` fired — whichever happens first. `label` names the server in the
/// timeout warning.
///
/// A server that is abandoned reports success: the deadline expiring is a
/// tolerated outcome of shutdown, not a startup/runtime failure, and the caller
/// still has to run its cleanup.
///
/// # Errors
///
/// Propagates the server's own error when it finishes before the deadline.
pub async fn drain_with_deadline<F>(
    server: F,
    shutdown: CancellationToken,
    deadline: Duration,
    label: &str,
) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    select! {
        result = server => result,
        () = async {
            shutdown.cancelled().await;

            sleep(deadline).await;
        } => {
            warn!(
                "{label}: graceful shutdown timed out after {}s",
                deadline.as_secs()
            );

            Ok(())
        }
    }
}

/// Shared "startup work has finished" flag.
///
/// Set once, by whichever component owns the last piece of startup work (the
/// scheduler's stale-job recovery), and read by the readiness probe. Until it
/// is set the process is live but not ready: it answers requests, yet a job
/// row it is about to reclaim may still look `running` to anything that reads
/// it, so an orchestrator must not route traffic here or consider a rolling
/// deploy's next step safe.
#[derive(Clone, Default)]
pub struct Readiness(Arc<AtomicBool>);

impl Readiness {
    /// A not-yet-ready flag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An already-ready flag, for a process with no deferred startup work.
    #[must_use]
    pub fn ready() -> Self {
        let readiness = Self::new();
        readiness.mark_ready();

        readiness
    }

    /// Mark startup complete. Idempotent.
    pub fn mark_ready(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether startup has completed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn readiness_starts_unready_and_latches() {
        let readiness = Readiness::new();
        assert!(!readiness.is_ready());

        let clone = readiness.clone();
        clone.mark_ready();

        assert!(
            readiness.is_ready(),
            "a clone shares the flag with its origin"
        );

        clone.mark_ready();
        assert!(readiness.is_ready(), "marking twice stays ready");

        assert!(Readiness::ready().is_ready());
    }

    #[tokio::test]
    async fn a_server_that_finishes_first_keeps_its_result() {
        let shutdown = CancellationToken::new();

        let ok = drain_with_deadline(
            async { Ok(()) },
            shutdown.clone(),
            Duration::from_secs(30),
            "test",
        )
        .await;
        assert!(ok.is_ok());

        let err = drain_with_deadline(
            async { Err(anyhow!("bind failed")) },
            shutdown,
            Duration::from_secs(30),
            "test",
        )
        .await;
        assert_eq!(err.unwrap_err().to_string(), "bind failed");
    }

    #[tokio::test]
    async fn a_hanging_server_is_abandoned_once_the_deadline_passes() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        let result = drain_with_deadline(
            std::future::pending::<Result<()>>(),
            shutdown,
            Duration::from_millis(20),
            "test",
        )
        .await;

        assert!(
            result.is_ok(),
            "an abandoned drain is a tolerated shutdown outcome"
        );
    }

    /// The deadline only starts once the token fires — a server still doing
    /// real work before any shutdown request must not be cut off.
    #[tokio::test]
    async fn the_deadline_does_not_run_before_the_token_fires() {
        let shutdown = CancellationToken::new();

        let result = drain_with_deadline(
            async {
                sleep(Duration::from_millis(60)).await;

                Ok(())
            },
            shutdown,
            Duration::from_millis(10),
            "test",
        )
        .await;

        assert!(result.is_ok(), "the server ran to completion");
    }
}
