//! The request scope, carried onto the blocking threads that do a request's
//! work.
//!
//! A request's handling runs on its task under two task-locals the admin
//! middleware enters: the viewer's label locale
//! ([`with_label_locale`](crate::core::with_label_locale)) and the request's
//! commit gate ([`with_commit_gate`](crate::core::with_commit_gate)).
//! Task-locals do not follow work onto a `spawn_blocking` thread, so
//! [`spawn_request_blocking`] captures both and re-enters them there. It is
//! the one way request work moves onto a blocking thread: a label resolved
//! there follows the viewer's UI locale, and a write there cannot commit once
//! the request was answered as timed out.
//!
//! Work that is detached from the request's answer — a fire-and-forget email
//! or MFA-code delivery — is spawned with plain `spawn_blocking` on purpose:
//! it must not be refused a commit because the request that started it has
//! since been answered.

use tokio::task::{JoinHandle, spawn_blocking};

use crate::core::{current_commit_gate, current_label_locale, in_commit_gate, in_label_locale};

/// Run `f` on a blocking thread inside the caller's request scope: its label
/// locale and its commit gate (see the module docs).
pub fn spawn_request_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let label_locale = current_label_locale();
    let gate = current_commit_gate();

    spawn_blocking(move || in_commit_gate(gate, || in_label_locale(label_locale, f)))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::core::{
        CommitGate, LocalizedString, request_deadline, with_commit_gate, with_label_locale,
    };

    /// Regression: work moved onto a blocking thread (every admin write, the
    /// edit page's read, restore, empty trash) resolved labels against the
    /// default locale instead of the viewer's UI locale.
    #[tokio::test]
    async fn blocking_work_keeps_the_caller_label_locale() {
        let ls = LocalizedString::Localized(
            [("de", "Titel"), ("en", "Title"), ("fr", "Titre")]
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        );

        let resolved = with_label_locale("fr".to_string(), async move {
            spawn_request_blocking(move || ls.resolve_current().to_string()).await
        });

        assert_eq!(resolved.await.unwrap(), "Titre");
    }

    /// The request's commit gate reaches the blocking thread with the label
    /// locale.
    #[tokio::test]
    async fn blocking_work_keeps_the_caller_commit_gate() {
        let gate = CommitGate::new(Instant::now() + Duration::from_mins(1));

        let deadline = with_commit_gate(gate.clone(), async {
            spawn_request_blocking(request_deadline).await
        });

        assert_eq!(deadline.await.unwrap(), Some(gate.deadline()));
    }

    /// Outside any request scope the blocking work runs unscoped.
    #[tokio::test]
    async fn blocking_work_without_a_scope_stays_unscoped() {
        let scope = spawn_request_blocking(|| (current_label_locale(), request_deadline())).await;

        assert_eq!(scope.unwrap(), (None, None));
    }
}
