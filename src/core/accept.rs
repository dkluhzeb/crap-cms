//! Accepting TCP connections under a connection cap — the one accept path
//! the admin HTTP listener and the gRPC listener share.

use std::{
    io::{Error as IoError, ErrorKind},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::sleep,
};
use tracing::{error, info, warn};

use crate::core::open_files::open_file_limits;

/// The HTTP/2 connection preface every prior-knowledge HTTP/2 client (h2c,
/// gRPC) opens with.
pub const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// How often the protocol sniff re-reads a connection that has sent only part
/// of the HTTP/2 preface.
const SNIFF_POLL: Duration = Duration::from_millis(10);

/// Pause after an accept error that is not about one connection (typically
/// the process running out of file descriptors), so the loop does not spin
/// while the condition lasts.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// Descriptors a derived connection cap leaves to everything that is not a
/// client connection: the database pools, log and upload files, outbound
/// SMTP / Redis / S3 connections.
const RESERVED_DESCRIPTORS: u64 = 256;

/// The listeners sharing the process's descriptors: admin HTTP and gRPC.
const LISTENERS: u64 = 2;

/// The least a derived cap is, however low the descriptor limit.
const MIN_DERIVED_CONNECTIONS: usize = 64;

/// The most a derived cap is, however high (or unlimited) the descriptor
/// limit: past this, per-connection memory matters more than descriptors.
const MAX_DERIVED_CONNECTIONS: usize = 65_536;

/// The cap where the platform reports no descriptor limit.
const FALLBACK_CONNECTIONS: usize = 4096;

/// Which HTTP version a connection opens with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpProtocol {
    Http1,
    Http2,
}

/// An accepted connection holding one slot of its listener's cap. The slot is
/// freed when `permit` is dropped, so it travels with the connection.
pub struct AcceptedConnection {
    pub stream: TcpStream,
    pub peer: SocketAddr,
    pub permit: OwnedSemaphorePermit,
}

/// The cap on concurrently open connections of one listener.
///
/// At the cap the listener stops accepting — new connections wait in the
/// kernel's backlog — instead of letting descriptors and per-connection
/// memory grow without bound.
#[derive(Clone)]
pub struct ConnectionCap {
    permits: Arc<Semaphore>,
}

impl ConnectionCap {
    /// A cap of `max_connections` (at least one, at most what a semaphore
    /// can count).
    #[must_use]
    pub fn new(max_connections: usize) -> Self {
        let slots = max_connections.clamp(1, Semaphore::MAX_PERMITS);

        Self {
            permits: Arc::new(Semaphore::new(slots)),
        }
    }

    /// Wait for a free slot, then accept the next connection on `listener`.
    ///
    /// Errors that concern a single connection are skipped; any other accept
    /// error is logged and retried after a short pause, so running out of
    /// descriptors degrades the listener instead of ending it.
    ///
    /// # Panics
    ///
    /// Never in practice: the semaphore is private to the cap and never
    /// closed, which is the only way acquiring a slot can fail.
    pub async fn accept(&self, listener: &TcpListener) -> AcceptedConnection {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .expect("the connection semaphore is never closed");

        let (stream, peer) = accept_retrying(listener).await;

        AcceptedConnection {
            stream,
            peer,
            permit,
        }
    }
}

/// The connection cap of one listener: `configured` (`[server]
/// max_connections`) when set, else derived from the process's open-file soft
/// limit ([`derive_max_connections`]). `listener` names the listener in the
/// startup log, which records the chosen value and where it came from.
#[must_use]
pub fn resolve_max_connections(configured: Option<usize>, listener: &str) -> usize {
    let soft_limit = soft_descriptor_limit();

    let Some(configured) = configured else {
        let derived = derive_max_connections(soft_limit);
        info!(
            max_connections = derived,
            open_file_limit = ?soft_limit,
            "{listener}: connection cap derived from the open-file limit"
        );

        return derived;
    };

    if soft_limit.is_some_and(|soft| exceeds_budget(configured, soft)) {
        warn!(
            max_connections = configured,
            open_file_limit = ?soft_limit,
            "{listener}: max_connections leaves too few descriptors for both listeners \
             and the database; accepts may fail with \"too many open files\""
        );
    }

    configured
}

/// The per-listener connection cap for an open-file soft limit of
/// `soft_limit`: what is left after [`RESERVED_DESCRIPTORS`], split between
/// the [`LISTENERS`], clamped to a sane range; [`FALLBACK_CONNECTIONS`] when
/// the platform reports no limit.
fn derive_max_connections(soft_limit: Option<u64>) -> usize {
    let Some(soft_limit) = soft_limit else {
        return FALLBACK_CONNECTIONS;
    };

    let per_listener = soft_limit.saturating_sub(RESERVED_DESCRIPTORS) / LISTENERS;

    usize::try_from(per_listener)
        .unwrap_or(MAX_DERIVED_CONNECTIONS)
        .clamp(MIN_DERIVED_CONNECTIONS, MAX_DERIVED_CONNECTIONS)
}

/// Whether `max_connections` per listener, plus the reserve, needs more
/// descriptors than `soft_limit` allows.
fn exceeds_budget(max_connections: usize, soft_limit: u64) -> bool {
    let wanted = u64::try_from(max_connections)
        .unwrap_or(u64::MAX)
        .saturating_mul(LISTENERS)
        .saturating_add(RESERVED_DESCRIPTORS);

    wanted > soft_limit
}

/// The process's open-file (`RLIMIT_NOFILE`) soft limit; `None` when it
/// cannot be read. An unlimited soft limit reads as `u64::MAX`.
fn soft_descriptor_limit() -> Option<u64> {
    open_file_limits().map(|(soft, _)| soft)
}

/// Accept one connection, riding out transient accept errors.
async fn accept_retrying(listener: &TcpListener) -> (TcpStream, SocketAddr) {
    loop {
        match listener.accept().await {
            Ok(accepted) => return accepted,
            Err(e) if is_connection_error(&e) => {}
            Err(e) => {
                error!("Accept error, retrying shortly: {e}");
                sleep(ACCEPT_ERROR_BACKOFF).await;
            }
        }
    }
}

/// Decide the protocol from the connection's first bytes without consuming
/// them: the HTTP/2 preface means HTTP/2, anything else HTTP/1. `None` when
/// the client closes (or errors) first. Callers bound it with a deadline — a
/// client that sends nothing, or stalls inside the preface, never answers.
pub async fn sniff_protocol(stream: &TcpStream) -> Option<HttpProtocol> {
    let mut buf = [0u8; H2_PREFACE.len()];

    loop {
        let read = match stream.peek(&mut buf).await {
            Ok(0) => return None,
            Ok(read) => read,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };

        if !H2_PREFACE.starts_with(&buf[..read]) {
            return Some(HttpProtocol::Http1);
        }

        if read == H2_PREFACE.len() {
            return Some(HttpProtocol::Http2);
        }

        // Part of the preface so far: `peek` would return the same bytes at
        // once, so wait before looking again.
        sleep(SNIFF_POLL).await;
    }
}

/// Errors that concern only the connection being accepted.
fn is_connection_error(e: &IoError) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset
    )
}

#[cfg(test)]
mod tests {
    use tokio::time::timeout;

    use super::*;

    async fn listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        (listener, addr)
    }

    /// At the cap no further connection is accepted until a slot frees.
    #[tokio::test]
    async fn a_full_cap_holds_the_next_connection_until_a_slot_frees() {
        let (listener, addr) = listener().await;
        let cap = ConnectionCap::new(1);

        let _first_client = TcpStream::connect(addr).await.unwrap();
        let first = cap.accept(&listener).await;

        let _second_client = TcpStream::connect(addr).await.unwrap();
        let waiting = timeout(Duration::from_millis(200), cap.accept(&listener)).await;
        assert!(waiting.is_err(), "the cap of one is taken");

        drop(first);
        let second = timeout(Duration::from_secs(5), cap.accept(&listener)).await;
        assert!(
            second.is_ok(),
            "the freed slot admits the waiting connection"
        );
    }

    #[test]
    fn the_derived_cap_splits_the_limit_left_after_the_reserve() {
        assert_eq!(
            derive_max_connections(Some(1024)),
            usize::try_from((1024 - RESERVED_DESCRIPTORS) / LISTENERS).unwrap()
        );
        assert_eq!(
            derive_max_connections(Some(8192 + RESERVED_DESCRIPTORS)),
            4096
        );
    }

    #[test]
    fn the_derived_cap_has_a_floor_and_a_ceiling() {
        assert_eq!(derive_max_connections(Some(0)), MIN_DERIVED_CONNECTIONS);
        assert_eq!(derive_max_connections(Some(300)), MIN_DERIVED_CONNECTIONS);
        assert_eq!(
            derive_max_connections(Some(u64::MAX)),
            MAX_DERIVED_CONNECTIONS
        );
    }

    #[test]
    fn without_a_known_limit_the_cap_is_the_fallback() {
        assert_eq!(derive_max_connections(None), FALLBACK_CONNECTIONS);
    }

    #[test]
    fn a_configured_cap_wins_over_the_derived_one() {
        assert_eq!(resolve_max_connections(Some(12), "test"), 12);
    }

    #[test]
    fn a_configured_cap_is_checked_against_the_descriptor_budget() {
        assert!(!exceeds_budget(384, 1024));
        assert!(exceeds_budget(385, 1024));
        assert!(exceeds_budget(usize::MAX, u64::MAX - 1));
    }

    /// The live limit is read on this platform (every CI target is unix).
    #[cfg(unix)]
    #[test]
    fn the_process_limit_is_readable() {
        assert!(soft_descriptor_limit().is_some_and(|limit| limit > 0));
    }

    #[test]
    fn a_zero_cap_still_admits_one_connection() {
        assert_eq!(ConnectionCap::new(0).permits.available_permits(), 1);
    }

    #[test]
    fn per_connection_errors_are_skipped_not_backed_off() {
        assert!(is_connection_error(&IoError::from(
            ErrorKind::ConnectionReset
        )));
        assert!(!is_connection_error(&IoError::other("too many open files")));
    }
}
