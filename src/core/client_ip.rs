//! The client address of a request, resolved once for every surface.
//!
//! [`ClientIp::resolve`] is the single place the admin HTTP server, custom
//! routes and the gRPC API turn a TCP peer plus an optional
//! `X-Forwarded-For` header into the address the request is attributed to;
//! [`ClientIp::rate_limit_key`] is the single place that address becomes a
//! per-IP rate-limit bucket.

use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    str,
};

use axum::http::HeaderMap;
use ipnet::{IpNet, Ipv6Net};

use crate::config::ServerConfig;

/// The forwarding header a trusted reverse proxy sets.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// Prefix length an IPv6 client is bucketed to for rate limiting. A single
/// host is routinely handed a whole /64, so keying on the full address would
/// give it 2^64 fresh budgets.
const IPV6_BUCKET_PREFIX: u8 = 64;

/// The wildcard `trusted_proxies` entry: every direct peer is a proxy.
const TRUST_ANY_PEER: &str = "*";

/// The address a request is attributed to, in canonical form (an
/// IPv4-mapped IPv6 address is reported as the IPv4 address it carries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(IpAddr);

impl ClientIp {
    /// Wrap an address, canonicalizing an IPv4-mapped IPv6 address to IPv4.
    #[must_use]
    pub fn new(addr: IpAddr) -> Self {
        Self(addr.to_canonical())
    }

    /// Resolve the client of a request whose TCP peer is `peer`.
    ///
    /// With `trust_proxy = false`, or when `peer` is not a trusted proxy, the
    /// peer itself is the client and `X-Forwarded-For` is ignored.
    ///
    /// Otherwise the `X-Forwarded-For` chain is walked from the RIGHT: each
    /// proxy appends the address it received the request from, so the
    /// rightmost entries are the ones trusted infrastructure vouches for and
    /// the leftmost is whatever the client chose to send. Entries that are
    /// themselves listed in `trusted_proxies` are skipped as further proxy
    /// hops; the first entry that is not is the client. The `"*"` wildcard
    /// only vouches for the direct peer — it never marks a forwarded entry as
    /// a proxy — so behind `["*"]` the rightmost entry is the client. When
    /// every entry is a listed proxy the leftmost is taken, and an entry that
    /// does not parse stops the walk at the nearest hop already accepted (the
    /// peer when there is none), so junk can never pick a bucket.
    #[must_use]
    pub fn resolve(headers: &HeaderMap, peer: IpAddr, server: &ServerConfig) -> Self {
        let peer = peer.to_canonical();

        if !server.trust_proxy || !peer_is_trusted(peer, &server.trusted_proxies) {
            return Self(peer);
        }

        Self(forwarded_client(headers, peer, &server.trusted_proxies))
    }

    /// The resolved address.
    #[must_use]
    pub fn addr(&self) -> IpAddr {
        self.0
    }

    /// The per-IP rate-limit bucket this client counts against: the IPv4
    /// address itself, or the IPv6 address's /64 network.
    ///
    /// Every per-IP limiter keys on this, never on the address, so a client
    /// cannot mint fresh budgets by rotating addresses inside its own /64.
    #[must_use]
    pub fn rate_limit_key(&self) -> String {
        let IpAddr::V6(v6) = self.0 else {
            return self.0.to_string();
        };

        Ipv6Net::new(v6, IPV6_BUCKET_PREFIX)
            .map_or_else(|_| v6.to_string(), |net| net.trunc().to_string())
    }
}

/// The full canonical address — what hooks and audit logs see. Rate limiting
/// uses [`ClientIp::rate_limit_key`] instead.
impl fmt::Display for ClientIp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Whether the direct peer may set `X-Forwarded-For`: it matches a listed
/// IP / CIDR, or the list contains the `"*"` wildcard.
fn peer_is_trusted(peer: IpAddr, trusted: &[String]) -> bool {
    trusted.iter().any(|entry| entry == TRUST_ANY_PEER) || is_listed_proxy(peer, trusted)
}

/// Whether `ip` (canonical) matches an explicit IP or CIDR entry (the
/// wildcard is not an entry here). An IPv4 address also matches a CIDR
/// written in IPv4-mapped IPv6 form (`::ffff:10.0.0.0/104`). Malformed
/// entries are refused at startup, so none should reach this; one that does
/// matches nothing.
fn is_listed_proxy(ip: IpAddr, trusted: &[String]) -> bool {
    let mapped = match ip {
        IpAddr::V4(v4) => IpAddr::V6(v4.to_ipv6_mapped()),
        IpAddr::V6(_) => ip,
    };

    trusted.iter().any(|entry| match entry.parse::<IpNet>() {
        Ok(net) => net.contains(&ip) || net.contains(&mapped),
        Err(_) => entry
            .parse::<IpAddr>()
            .is_ok_and(|a| a.to_canonical() == ip),
    })
}

/// Walk the forwarded chain right-to-left; see [`ClientIp::resolve`].
fn forwarded_client(headers: &HeaderMap, peer: IpAddr, trusted: &[String]) -> IpAddr {
    let mut nearest = peer;

    for entry in forwarded_entries(headers).into_iter().rev() {
        let Some(ip) = str::from_utf8(entry).ok().and_then(parse_entry) else {
            return nearest;
        };

        if !is_listed_proxy(ip, trusted) {
            return ip;
        }

        nearest = ip;
    }

    nearest
}

/// Every `X-Forwarded-For` entry in order, across repeated header lines
/// (which HTTP defines as one comma-joined list), as raw bytes.
///
/// Lines are split before any text decoding: a byte the client smuggled into
/// its own (leftmost) entry must spoil that entry only, never the entries the
/// proxies appended to the same line.
fn forwarded_entries(headers: &HeaderMap) -> Vec<&[u8]> {
    headers
        .get_all(FORWARDED_FOR)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .map(<[u8]>::trim_ascii)
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// One chain entry as an address — bare (`203.0.113.5`, `2001:db8::1`) or
/// with the port some proxies append (`203.0.113.5:4711`, `[2001:db8::1]:4711`).
fn parse_entry(entry: &str) -> Option<IpAddr> {
    let ip = entry
        .parse::<IpAddr>()
        .ok()
        .or_else(|| entry.parse::<SocketAddr>().ok().map(|s| s.ip()))?;

    Some(ip.to_canonical())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(xff: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();

        for value in xff {
            map.append(FORWARDED_FOR, value.parse().unwrap());
        }

        map
    }

    fn trusting(entries: &[&str]) -> ServerConfig {
        ServerConfig {
            trust_proxy: true,
            trusted_proxies: entries.iter().map(ToString::to_string).collect(),
            ..ServerConfig::default()
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn resolve(xff: &[&str], peer: &str, server: &ServerConfig) -> String {
        ClientIp::resolve(&headers(xff), ip(peer), server).to_string()
    }

    #[test]
    fn without_trust_proxy_the_peer_is_the_client() {
        let server = ServerConfig::default();

        assert_eq!(resolve(&["10.0.0.1"], "127.0.0.1", &server), "127.0.0.1");
    }

    #[test]
    fn an_untrusted_peer_cannot_set_the_client() {
        let server = trusting(&["10.0.0.0/8"]);

        assert_eq!(resolve(&["203.0.113.5"], "1.2.3.4", &server), "1.2.3.4");
    }

    #[test]
    fn a_trusted_peer_vouches_for_its_single_entry() {
        let server = trusting(&["10.0.0.0/8"]);

        assert_eq!(
            resolve(&["203.0.113.5"], "10.0.0.5", &server),
            "203.0.113.5"
        );
    }

    /// Regression: an appending proxy (nginx `$proxy_add_x_forwarded_for`)
    /// keeps whatever the client sent and adds the real peer on the right.
    /// Taking the leftmost entry let every request pick its own rate-limit
    /// bucket by sending a fresh fake address.
    #[test]
    fn a_spoofed_leftmost_entry_behind_an_appending_proxy_is_ignored() {
        let server = trusting(&["10.0.0.0/8"]);

        assert_eq!(
            resolve(&["198.51.100.77, 203.0.113.5"], "10.0.0.5", &server),
            "203.0.113.5"
        );
    }

    #[test]
    fn listed_proxy_hops_are_skipped_right_to_left() {
        let server = trusting(&["10.0.0.0/8", "192.168.0.0/16"]);

        assert_eq!(
            resolve(
                &["198.51.100.77, 203.0.113.5, 192.168.1.1, 10.1.2.3"],
                "10.0.0.5",
                &server
            ),
            "203.0.113.5"
        );
    }

    /// The wildcard trusts the direct peer only; it never turns a forwarded
    /// entry into a proxy hop, so the rightmost entry is the client.
    #[test]
    fn the_wildcard_takes_the_rightmost_entry() {
        let server = trusting(&["*"]);

        assert_eq!(
            resolve(&["198.51.100.77, 203.0.113.5"], "1.2.3.4", &server),
            "203.0.113.5"
        );
    }

    #[test]
    fn repeated_header_lines_form_one_chain() {
        let server = trusting(&["10.0.0.0/8"]);

        assert_eq!(
            resolve(&["198.51.100.77", "203.0.113.5"], "10.0.0.5", &server),
            "203.0.113.5"
        );
    }

    #[test]
    fn a_chain_of_only_proxies_yields_the_leftmost() {
        let server = trusting(&["10.0.0.0/8"]);

        assert_eq!(
            resolve(&["10.9.9.9, 10.1.1.1"], "10.0.0.5", &server),
            "10.9.9.9"
        );
    }

    #[test]
    fn junk_stops_the_walk_at_the_nearest_accepted_hop() {
        let server = trusting(&["10.0.0.0/8"]);

        assert_eq!(resolve(&["not-an-ip"], "10.0.0.5", &server), "10.0.0.5");
        assert_eq!(
            resolve(&["not-an-ip, 10.1.1.1"], "10.0.0.5", &server),
            "10.1.1.1"
        );
    }

    /// Regression: a header line with a byte that is not visible ASCII was
    /// dropped whole, taking the address an appending proxy had added to it
    /// along — the client got the proxy's bucket. The junk is only one entry,
    /// left of the proxy's, so the walk never reaches it.
    #[test]
    fn a_non_ascii_entry_the_client_sent_does_not_hide_the_proxy_entry() {
        let server = trusting(&["10.0.0.0/8"]);
        let mut map = HeaderMap::new();
        map.append(
            FORWARDED_FOR,
            HeaderValue::from_bytes(b"\xfe\xff, 203.0.113.5").unwrap(),
        );

        assert_eq!(
            ClientIp::resolve(&map, ip("10.0.0.5"), &server).to_string(),
            "203.0.113.5"
        );
    }

    #[test]
    fn an_empty_or_missing_header_falls_back_to_the_peer() {
        let server = trusting(&["*"]);

        assert_eq!(resolve(&[""], "10.0.0.2", &server), "10.0.0.2");
        assert_eq!(resolve(&[], "10.0.0.2", &server), "10.0.0.2");
    }

    #[test]
    fn entries_with_ports_parse() {
        let server = trusting(&["*"]);

        assert_eq!(
            resolve(&["203.0.113.5:4711"], "10.0.0.5", &server),
            "203.0.113.5"
        );
        assert_eq!(
            resolve(&["[2001:db8::1]:4711"], "10.0.0.5", &server),
            "2001:db8::1"
        );
    }

    #[test]
    fn ipv6_entries_are_canonical() {
        let server = trusting(&["::1/128"]);

        assert_eq!(
            resolve(&["2001:0db8:0000:0000:0000:0000:0000:0001"], "::1", &server),
            "2001:db8::1"
        );
    }

    #[test]
    fn ipv4_mapped_addresses_are_reported_as_ipv4() {
        let server = trusting(&["10.0.0.0/8"]);

        assert_eq!(
            resolve(&["::ffff:203.0.113.5"], "::ffff:10.0.0.5", &server),
            "203.0.113.5"
        );
        assert_eq!(ClientIp::new(ip("::ffff:1.2.3.4")).to_string(), "1.2.3.4");
    }

    #[test]
    fn an_ipv4_mapped_allowlist_cidr_matches_an_ipv4_peer() {
        let server = trusting(&["::ffff:10.0.0.0/104"]);

        assert_eq!(
            resolve(&["203.0.113.5"], "10.0.0.5", &server),
            "203.0.113.5"
        );
    }

    #[test]
    fn a_malformed_allowlist_entry_matches_nothing() {
        let server = trusting(&["not-a-cidr", "10.0.0.0/8"]);

        assert_eq!(
            resolve(&["203.0.113.5"], "10.0.0.5", &server),
            "203.0.113.5"
        );
    }

    #[test]
    fn ipv4_buckets_by_address() {
        assert_eq!(
            ClientIp::new(ip("203.0.113.5")).rate_limit_key(),
            "203.0.113.5"
        );
    }

    /// Regression: per-IP limiters keyed IPv6 per /128, so one host with a
    /// /64 had 2^64 fresh budgets.
    #[test]
    fn ipv6_buckets_by_slash_64() {
        let a = ClientIp::new(ip("2001:db8:1:2::1")).rate_limit_key();
        let b = ClientIp::new(ip("2001:db8:1:2:ffff:ffff:ffff:fffe")).rate_limit_key();
        let other = ClientIp::new(ip("2001:db8:1:3::1")).rate_limit_key();

        assert_eq!(a, "2001:db8:1:2::/64");
        assert_eq!(a, b);
        assert_ne!(a, other);
    }

    #[test]
    fn ipv4_mapped_ipv6_buckets_as_ipv4() {
        assert_eq!(
            ClientIp::new(ip("::ffff:203.0.113.5")).rate_limit_key(),
            "203.0.113.5"
        );
    }
}
