//! The admin HTTP accept loop: connection cap, header-read deadline, and a
//! graceful drain on shutdown.
//!
//! Every admin connection — HTTP/1.1, or HTTP/2 cleartext when `h2c` is on —
//! is served here, so the limits hold whichever protocol a client speaks.

use std::time::Duration;

use anyhow::Result;
use axum::{Router, extract::ConnectInfo};
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::{
        conn::auto::Builder as AutoBuilder,
        graceful::{GracefulShutdown, Watcher},
    },
};
use tokio::{net::TcpListener, select, spawn, time::timeout};
use tokio_util::sync::CancellationToken;
use tower::Service;

use crate::{
    config::ServerConfig,
    core::{
        AcceptedConnection, ConnectionCap, HttpProtocol, resolve_max_connections, sniff_protocol,
    },
};

/// Interval of the HTTP/2 keep-alive ping that detects dead h2c peers.
const H2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_mins(1);

/// The limits one admin listener enforces.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ListenerLimits {
    /// Most connections open at once; at the cap, accepting pauses.
    max_connections: usize,
    /// How long a client may take to send a request's headers — and, with
    /// `h2c`, to show which protocol it speaks.
    header_read_timeout: Duration,
}

impl ListenerLimits {
    /// Limits of `max_connections` open connections and `header_read_timeout`
    /// to send a request's headers.
    #[must_use]
    pub(crate) fn new(max_connections: usize, header_read_timeout: Duration) -> Self {
        Self {
            max_connections,
            header_read_timeout,
        }
    }

    /// The limits `[server]` configures, the connection cap derived from the
    /// open-file limit when `max_connections` is unset.
    #[must_use]
    pub(crate) fn from_config(server: &ServerConfig) -> Self {
        Self::new(
            resolve_max_connections(server.max_connections, "Admin listener"),
            Duration::from_secs(server.header_read_timeout),
        )
    }
}

/// The per-protocol connection builders, configured once per listener.
#[derive(Clone)]
struct Builders {
    http1: AutoBuilder<TokioExecutor>,
    http2: AutoBuilder<TokioExecutor>,
}

impl Builders {
    /// HTTP/1 with the header-read deadline; HTTP/2 with keep-alive pings.
    fn new(limits: ListenerLimits) -> Self {
        let mut base = AutoBuilder::new(TokioExecutor::new());

        base.http1()
            .timer(TokioTimer::new())
            .header_read_timeout(limits.header_read_timeout);

        // hyper's HTTP/2 server has no idle timeout: the pings close a dead
        // peer, but an idle one that answers them keeps its connection (and
        // its slot) until it closes it — documented with `h2c`, which is
        // meant for a fronting proxy.
        base.http2()
            .timer(TokioTimer::new())
            .keep_alive_interval(H2_KEEP_ALIVE_INTERVAL);

        Self {
            http1: base.clone().http1_only(),
            http2: base.http2_only(),
        }
    }

    fn for_protocol(&self, protocol: HttpProtocol) -> &AutoBuilder<TokioExecutor> {
        match protocol {
            HttpProtocol::Http1 => &self.http1,
            HttpProtocol::Http2 => &self.http2,
        }
    }
}

/// Serve `app` on `listener` until `shutdown` fires, then stop accepting and
/// let open connections finish (the caller bounds that drain).
///
/// With `h2c` off every connection is HTTP/1.1, whose header-read deadline
/// runs from the moment the connection opens. With `h2c` on, the protocol is
/// sniffed first under the same deadline, so a client that never sends a
/// byte — or stalls inside the HTTP/2 preface — cannot hold a slot.
pub(crate) async fn serve(
    listener: TcpListener,
    app: Router,
    limits: ListenerLimits,
    h2c: bool,
    shutdown: CancellationToken,
) -> Result<()> {
    let cap = ConnectionCap::new(limits.max_connections);
    let builders = Builders::new(limits);
    let graceful = GracefulShutdown::new();

    loop {
        let accepted = select! {
            accepted = cap.accept(&listener) => accepted,
            () = shutdown.cancelled() => break,
        };

        let connection = Connection {
            accepted,
            app: app.clone(),
            builders: builders.clone(),
            watcher: graceful.watcher(),
        };

        spawn(connection.serve(limits.header_read_timeout, h2c));
    }

    drop(listener);
    graceful.shutdown().await;

    Ok(())
}

/// One accepted connection and everything needed to serve it.
struct Connection {
    accepted: AcceptedConnection,
    app: Router,
    builders: Builders,
    watcher: Watcher,
}

impl Connection {
    /// Serve the connection to completion; its cap slot frees when this
    /// returns. Connection errors (client disconnects, timeouts) are expected
    /// and not reported.
    async fn serve(self, header_read_timeout: Duration, h2c: bool) {
        let Self {
            accepted,
            app,
            builders,
            watcher,
        } = self;
        let AcceptedConnection {
            stream,
            peer,
            permit,
        } = accepted;

        let protocol = if h2c {
            match timeout(header_read_timeout, sniff_protocol(&stream)).await {
                Ok(Some(protocol)) => protocol,
                Ok(None) | Err(_) => return,
            }
        } else {
            HttpProtocol::Http1
        };

        // `ConnectInfo` is what the handlers' client-address extraction reads.
        let service = service_fn(move |mut req| {
            req.extensions_mut().insert(ConnectInfo(peer));
            app.clone().call(req)
        });

        let conn = builders
            .for_protocol(protocol)
            .serve_connection(TokioIo::new(stream), service);

        watcher.watch(conn).await.ok();

        drop(permit);
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::routing::get;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        time::sleep,
    };

    use super::*;
    use crate::core::H2_PREFACE;

    const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";

    async fn start(limits: ListenerLimits, h2c: bool) -> (SocketAddr, CancellationToken) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/", get(|| async { "ok" }));
        let shutdown = CancellationToken::new();

        spawn(serve(listener, app, limits, h2c, shutdown.clone()));

        (addr, shutdown)
    }

    /// Read until the server closes, failing the test after `limit`.
    async fn read_to_close(stream: &mut TcpStream, limit: Duration) -> String {
        let mut out = Vec::new();

        timeout(limit, stream.read_to_end(&mut out))
            .await
            .expect("the server must close the connection")
            .ok();

        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn a_complete_request_is_served() {
        let (addr, _shutdown) = start(ListenerLimits::new(8, Duration::from_secs(5)), false).await;
        let mut client = TcpStream::connect(addr).await.unwrap();

        client.write_all(REQUEST).await.unwrap();

        let response = read_to_close(&mut client, Duration::from_secs(5)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }

    /// Regression: the admin listener had no header-read deadline, so a
    /// client trickling (or never finishing) its headers held its connection
    /// forever.
    #[tokio::test]
    async fn a_stalled_header_is_cut_off() {
        let limits = ListenerLimits::new(8, Duration::from_millis(200));
        let (addr, _shutdown) = start(limits, false).await;
        let mut client = TcpStream::connect(addr).await.unwrap();

        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
            .await
            .unwrap();

        read_to_close(&mut client, Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn a_silent_connection_is_cut_off() {
        let limits = ListenerLimits::new(8, Duration::from_millis(200));

        for h2c in [false, true] {
            let (addr, _shutdown) = start(limits, h2c).await;
            let mut client = TcpStream::connect(addr).await.unwrap();

            read_to_close(&mut client, Duration::from_secs(5)).await;
        }
    }

    /// With h2c on, a client that stalls inside the HTTP/2 preface is cut off
    /// by the same deadline.
    #[tokio::test]
    async fn a_stalled_h2c_preface_is_cut_off() {
        let limits = ListenerLimits::new(8, Duration::from_millis(200));
        let (addr, _shutdown) = start(limits, true).await;
        let mut client = TcpStream::connect(addr).await.unwrap();

        client.write_all(&H2_PREFACE[..5]).await.unwrap();

        read_to_close(&mut client, Duration::from_secs(5)).await;
    }

    /// Regression: the admin listener accepted without limit. At the cap a
    /// further client waits until an open connection closes.
    #[tokio::test]
    async fn at_the_cap_a_client_waits_for_a_free_slot() {
        let (addr, _shutdown) = start(ListenerLimits::new(1, Duration::from_secs(5)), false).await;

        let holder = TcpStream::connect(addr).await.unwrap();
        sleep(Duration::from_millis(100)).await;

        let mut waiting = TcpStream::connect(addr).await.unwrap();
        waiting.write_all(REQUEST).await.unwrap();

        let mut first = [0u8; 1];
        let early = timeout(Duration::from_millis(300), waiting.read(&mut first)).await;
        assert!(early.is_err(), "no response while the only slot is taken");

        drop(holder);

        let response = read_to_close(&mut waiting, Duration::from_secs(5)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }

    #[tokio::test]
    async fn shutdown_stops_accepting() {
        let (addr, shutdown) = start(ListenerLimits::new(8, Duration::from_secs(5)), false).await;

        shutdown.cancel();
        sleep(Duration::from_millis(100)).await;

        assert!(TcpStream::connect(addr).await.is_err());
    }

    #[test]
    fn limits_come_from_the_server_config() {
        let server = ServerConfig {
            max_connections: Some(12),
            header_read_timeout: 7,
            ..ServerConfig::default()
        };

        let limits = ListenerLimits::from_config(&server);

        assert_eq!(limits.max_connections, 12);
        assert_eq!(limits.header_read_timeout, Duration::from_secs(7));
    }
}
