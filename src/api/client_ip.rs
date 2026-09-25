//! The client address of a gRPC request.
//!
//! Both the per-IP rate-limit layer (which sees the raw HTTP/2 request) and
//! the auth handlers (which see the decoded `tonic::Request`) resolve the
//! client here, through the same [`ClientIp::resolve`] the admin server uses,
//! so `trust_proxy` / `trusted_proxies` mean the same thing on every surface.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::http::{self, HeaderMap};
use tonic::{Request, transport::server::TcpConnectInfo};

use crate::{config::ServerConfig, core::ClientIp};

/// Stand-in peer for a request that reached no TCP acceptor (an in-process
/// call): every such request shares one bucket.
const UNKNOWN_PEER: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// Resolve the client from the request headers and the TCP peer, if any.
fn resolve(headers: &HeaderMap, peer: Option<SocketAddr>, server: &ServerConfig) -> ClientIp {
    let peer = peer.map_or(UNKNOWN_PEER, |addr| addr.ip());

    ClientIp::resolve(headers, peer, server)
}

/// The client of a decoded gRPC request.
pub(crate) fn request_client_ip<T>(request: &Request<T>, server: &ServerConfig) -> ClientIp {
    resolve(request.metadata().as_ref(), request.remote_addr(), server)
}

/// The client of a raw HTTP/2 request, before tonic decodes it.
pub(crate) fn http_client_ip<B>(request: &http::Request<B>, server: &ServerConfig) -> ClientIp {
    let peer = request
        .extensions()
        .get::<TcpConnectInfo>()
        .and_then(TcpConnectInfo::remote_addr);

    resolve(request.headers(), peer, server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusting_proxy() -> ServerConfig {
        ServerConfig {
            trust_proxy: true,
            trusted_proxies: vec!["10.0.0.0/8".to_string()],
            ..ServerConfig::default()
        }
    }

    fn connect_info(peer: &str) -> TcpConnectInfo {
        TcpConnectInfo {
            local_addr: None,
            remote_addr: Some(peer.parse().unwrap()),
        }
    }

    #[test]
    fn a_decoded_request_honors_a_trusted_proxy_like_the_admin_server() {
        let mut request = Request::new(());
        request
            .extensions_mut()
            .insert(connect_info("10.0.0.5:4000"));
        request.metadata_mut().insert(
            "x-forwarded-for",
            "198.51.100.7, 203.0.113.5".parse().unwrap(),
        );

        assert_eq!(
            request_client_ip(&request, &trusting_proxy()).to_string(),
            "203.0.113.5"
        );
        assert_eq!(
            request_client_ip(&request, &ServerConfig::default()).to_string(),
            "10.0.0.5"
        );
    }

    #[test]
    fn a_raw_request_resolves_through_the_same_rule() {
        let mut request = http::Request::new(());
        request
            .extensions_mut()
            .insert(connect_info("10.0.0.5:4000"));
        request.headers_mut().insert(
            "x-forwarded-for",
            "198.51.100.7, 203.0.113.5".parse().unwrap(),
        );

        assert_eq!(
            http_client_ip(&request, &trusting_proxy()).to_string(),
            "203.0.113.5"
        );
    }

    #[test]
    fn a_request_without_a_peer_shares_the_unknown_bucket() {
        let request = Request::new(());

        assert_eq!(
            request_client_ip(&request, &ServerConfig::default()).addr(),
            UNKNOWN_PEER
        );
    }
}
