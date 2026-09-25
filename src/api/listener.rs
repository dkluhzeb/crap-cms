//! The gRPC listener: connections accepted under the shared connection cap
//! ([`ConnectionCap`]) and handed to tonic as a stream once they have opened
//! with the HTTP/2 preface.

use std::{
    io::{IoSlice, Result as IoResult},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{
        OwnedSemaphorePermit,
        mpsc::{Sender, channel},
    },
    time::timeout,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::server::{Connected, TcpConnectInfo};

use crate::core::{AcceptedConnection, ConnectionCap, HttpProtocol, sniff_protocol};

/// An accepted gRPC connection that holds one slot of the listener's cap
/// until it is dropped. Reads and writes go straight to the socket; the
/// connection info (peer address) is the socket's, so per-IP limiting and
/// `remote_addr()` see the same client they always did.
pub(crate) struct CappedStream {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
}

impl From<AcceptedConnection> for CappedStream {
    fn from(accepted: AcceptedConnection) -> Self {
        Self {
            stream: accepted.stream,
            _permit: accepted.permit,
        }
    }
}

impl Connected for CappedStream {
    type ConnectInfo = TcpConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.stream.connect_info()
    }
}

impl AsyncRead for CappedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<IoResult<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for CappedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<IoResult<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<IoResult<usize>> {
        Pin::new(&mut self.stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Accept on `listener` under `cap` for as long as the returned stream is
/// being consumed. When tonic drops the stream (shutdown), accepting stops.
///
/// A connection reaches tonic only once it has sent the HTTP/2 preface; one
/// that sends nothing, stalls inside it, or opens with anything else within
/// `preface_timeout` is closed, so silent clients cannot sit on the cap's
/// slots (the HTTP/2 keep-alive pings only start after the handshake).
pub(crate) fn capped_incoming(
    listener: TcpListener,
    cap: ConnectionCap,
    preface_timeout: Duration,
) -> ReceiverStream<IoResult<CappedStream>> {
    let (tx, rx) = channel(1);

    spawn(accept_into(listener, cap, preface_timeout, tx));

    ReceiverStream::new(rx)
}

/// The accept task behind [`capped_incoming`]. Each connection waits for its
/// preface on its own task, so a slow one never holds up the next accept.
async fn accept_into(
    listener: TcpListener,
    cap: ConnectionCap,
    preface_timeout: Duration,
    tx: Sender<IoResult<CappedStream>>,
) {
    loop {
        let accepted = select! {
            accepted = cap.accept(&listener) => accepted,
            () = tx.closed() => return,
        };

        // gRPC is latency-sensitive: small frames go out immediately.
        accepted.stream.set_nodelay(true).ok();

        spawn(hand_over(accepted, preface_timeout, tx.clone()));
    }
}

/// Hand `accepted` to tonic once it has sent the HTTP/2 preface within
/// `preface_timeout`; otherwise drop it, freeing its slot.
async fn hand_over(
    accepted: AcceptedConnection,
    preface_timeout: Duration,
    tx: Sender<IoResult<CappedStream>>,
) {
    let protocol = timeout(preface_timeout, sniff_protocol(&accepted.stream)).await;

    if !matches!(protocol, Ok(Some(HttpProtocol::Http2))) {
        return;
    }

    tx.send(Ok(CappedStream::from(accepted))).await.ok();
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::io::AsyncWriteExt;
    use tokio_stream::StreamExt;

    use super::*;
    use crate::core::H2_PREFACE;

    const PREFACE_TIMEOUT: Duration = Duration::from_secs(5);

    /// A client that has opened with the HTTP/2 preface.
    async fn grpc_client(addr: SocketAddr) -> TcpStream {
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(H2_PREFACE).await.unwrap();

        client
    }

    /// Regression: the gRPC listener accepted without limit. At the cap the
    /// next connection is not handed to the server until a slot frees.
    #[tokio::test]
    async fn the_incoming_stream_honors_the_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut incoming = capped_incoming(listener, ConnectionCap::new(1), PREFACE_TIMEOUT);

        let first_client = grpc_client(addr).await;
        let first = incoming.next().await.unwrap().unwrap();
        assert_eq!(
            first.connect_info().remote_addr(),
            Some(first_client.local_addr().unwrap())
        );

        let _second_client = grpc_client(addr).await;
        let waiting = timeout(Duration::from_millis(200), incoming.next()).await;
        assert!(waiting.is_err(), "the cap of one is taken");

        drop(first);
        let second = timeout(Duration::from_secs(5), incoming.next()).await;
        assert!(second.is_ok(), "the freed slot admits the next connection");
    }

    /// Regression: a connection that never sent a byte was handed to tonic,
    /// whose HTTP/2 handshake waits for the preface without a deadline, so
    /// silent clients held the cap's slots forever. Past the preface deadline
    /// such a connection is closed and its slot admits the next client.
    #[tokio::test]
    async fn a_silent_connection_is_closed_and_frees_its_slot() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut incoming =
            capped_incoming(listener, ConnectionCap::new(1), Duration::from_millis(200));

        let _silent = TcpStream::connect(addr).await.unwrap();
        let _next = grpc_client(addr).await;

        let admitted = timeout(Duration::from_secs(5), incoming.next()).await;
        assert!(
            admitted.is_ok(),
            "the silent client's slot went to the next client"
        );
    }

    #[tokio::test]
    async fn a_connection_that_does_not_speak_http2_is_not_handed_over() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut incoming = capped_incoming(listener, ConnectionCap::new(8), PREFACE_TIMEOUT);

        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();

        let handed = timeout(Duration::from_millis(300), incoming.next()).await;
        assert!(handed.is_err(), "no HTTP/1 connection reaches tonic");
    }
}
