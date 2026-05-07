//! Receiver halves of the event / invalidation streams.
//!
//! In-process transports expose the broadcast channel directly. Redis
//! transports use a bounded mpsc channel fed by a background pump task; the
//! task signals overflow via a sentinel `Lagged` variant so the receiver
//! observes the same `RecvError::Lagged` as the in-process path.

use tokio::sync::broadcast;
#[cfg(feature = "redis")]
use tokio::sync::mpsc;

use super::types::MutationEvent;

/// Reasons a receiver could fail to deliver the next message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// The subscriber fell behind — one or more events were dropped. The inner
    /// value is the number of events that were skipped.
    Lagged(u64),
    /// The transport has been closed; no further events will be delivered.
    Closed,
}

impl std::fmt::Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvError::Lagged(n) => write!(f, "receiver lagged by {} events", n),
            RecvError::Closed => write!(f, "receiver closed"),
        }
    }
}

impl std::error::Error for RecvError {}

impl From<broadcast::error::RecvError> for RecvError {
    fn from(err: broadcast::error::RecvError) -> Self {
        match err {
            broadcast::error::RecvError::Lagged(n) => RecvError::Lagged(n),
            broadcast::error::RecvError::Closed => RecvError::Closed,
        }
    }
}

/// Internal receiver backing for [`EventReceiver`] / [`InvalidationReceiver`].
enum RecvKind<T: Clone + Send + 'static> {
    Broadcast(broadcast::Receiver<T>),
    /// Only constructed on the Redis transport path; the variant only
    /// exists when the `redis` feature is on.
    #[cfg(feature = "redis")]
    Mpsc(mpsc::Receiver<RemoteMessage<T>>),
}

/// Message type carried over the mpsc channel from a remote-transport pump task
/// to a local subscriber. The task either delivers a payload, or signals that
/// the local bounded queue overflowed so the subscriber should be dropped with
/// the same `Lagged` semantic as in-process broadcasts.
#[cfg(feature = "redis")]
#[derive(Clone)]
pub(crate) enum RemoteMessage<T: Clone + Send + 'static> {
    Event(T),
    Lagged(u64),
}

/// A receiver for mutation events. Mirrors `broadcast::Receiver` semantics
/// (error on lag) regardless of whether the underlying transport is in-process
/// or Redis pub/sub.
pub struct EventReceiver {
    inner: RecvKind<MutationEvent>,
}

impl EventReceiver {
    pub(crate) fn from_broadcast(rx: broadcast::Receiver<MutationEvent>) -> Self {
        Self {
            inner: RecvKind::Broadcast(rx),
        }
    }

    #[cfg(feature = "redis")]
    pub(crate) fn from_mpsc(rx: mpsc::Receiver<RemoteMessage<MutationEvent>>) -> Self {
        Self {
            inner: RecvKind::Mpsc(rx),
        }
    }

    /// Await the next event. Returns `Err(RecvError::Lagged(n))` if the
    /// subscriber fell behind (same semantic as `broadcast::Receiver::recv`).
    pub async fn recv(&mut self) -> Result<MutationEvent, RecvError> {
        match &mut self.inner {
            RecvKind::Broadcast(rx) => rx.recv().await.map_err(RecvError::from),
            #[cfg(feature = "redis")]
            RecvKind::Mpsc(rx) => match rx.recv().await {
                Some(RemoteMessage::Event(ev)) => Ok(ev),
                Some(RemoteMessage::Lagged(n)) => Err(RecvError::Lagged(n)),
                None => Err(RecvError::Closed),
            },
        }
    }
}

/// A receiver for user-invalidation signals. Same shape as [`EventReceiver`],
/// payload is the user document ID string.
pub struct InvalidationReceiver {
    inner: RecvKind<String>,
}

impl InvalidationReceiver {
    pub(crate) fn from_broadcast(rx: broadcast::Receiver<String>) -> Self {
        Self {
            inner: RecvKind::Broadcast(rx),
        }
    }

    #[cfg(feature = "redis")]
    pub(crate) fn from_mpsc(rx: mpsc::Receiver<RemoteMessage<String>>) -> Self {
        Self {
            inner: RecvKind::Mpsc(rx),
        }
    }

    /// Await the next invalidation signal. Returns `Err(RecvError::Lagged(n))`
    /// if the subscriber fell behind.
    pub async fn recv(&mut self) -> Result<String, RecvError> {
        match &mut self.inner {
            RecvKind::Broadcast(rx) => rx.recv().await.map_err(RecvError::from),
            #[cfg(feature = "redis")]
            RecvKind::Mpsc(rx) => match rx.recv().await {
                Some(RemoteMessage::Event(s)) => Ok(s),
                Some(RemoteMessage::Lagged(n)) => Err(RecvError::Lagged(n)),
                None => Err(RecvError::Closed),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recv_error_from_broadcast_lagged() {
        let err: RecvError = broadcast::error::RecvError::Lagged(7).into();
        assert_eq!(err, RecvError::Lagged(7));
    }

    #[test]
    fn recv_error_from_broadcast_closed() {
        let err: RecvError = broadcast::error::RecvError::Closed.into();
        assert_eq!(err, RecvError::Closed);
    }

    #[test]
    fn recv_error_display() {
        assert_eq!(
            RecvError::Lagged(3).to_string(),
            "receiver lagged by 3 events"
        );
        assert_eq!(RecvError::Closed.to_string(), "receiver closed");
    }
}
