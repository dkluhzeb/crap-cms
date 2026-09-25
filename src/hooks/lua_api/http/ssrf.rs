//! SSRF policy for `crap.http`: which addresses an outbound request may
//! reach when private networks are not allowed, and the vetted address a
//! client is pinned to.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    result::Result as StdResult,
};

use tracing::warn;
use url::Url;

/// Resolve and validate a URL against SSRF policy.
/// Returns `(hostname, SocketAddr)` — caller pins via `ClientBuilder::resolve()`.
pub(super) fn validate_url(url_str: &str) -> StdResult<(String, SocketAddr), String> {
    let parsed = Url::parse(url_str).map_err(|e| format!("invalid URL: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("unsupported scheme: {s}")),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?
        .to_string();

    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs: Vec<SocketAddr> = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed: {e}"))?
        .collect();

    // Find first non-private address to pin
    if let Some(&addr) = addrs.iter().find(|a| !is_private_ip(a.ip())) {
        return Ok((host, addr));
    }

    match addrs.first() {
        Some(addr) => Err(blocked(url_str, &host, addr.ip())),
        None => Err("DNS resolution returned no addresses".to_string()),
    }
}

/// Every resolved address was non-public. Log the concrete reason for
/// operators, but return a redacted error to the Lua caller — the caller
/// could be attacker-controlled and would otherwise enumerate internal IP
/// topology from these messages.
fn blocked(url_str: &str, host: &str, ip: IpAddr) -> String {
    let class = if ip.is_loopback() {
        "loopback"
    } else if ip.is_unspecified() {
        "unspecified"
    } else {
        "private"
    };

    warn!(
        url = %url_str,
        host = %host,
        resolved_ip = %ip,
        class = class,
        "crap.http: blocking request — target resolves to non-public address"
    );

    "Target resolves to a blocked address; see server logs for details".to_string()
}

/// Non-public IPv4 ranges: loopback, "this network" (0.0.0.0/8), RFC 1918
/// private, link-local, CGNAT (100.64.0.0/10 — Tailscale/fly.io internal
/// networks), IETF protocol assignments (192.0.0.0/24), and benchmarking
/// (198.18.0.0/15).
fn is_private_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();

    v4.is_loopback()
        || o[0] == 0
        || v4.is_private()
        || v4.is_link_local()
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
}

/// The IPv4 address an IPv6 address carries to a v4 host, if it is one of
/// the embedding forms a host or gateway translates: IPv4-mapped
/// (`::ffff:a.b.c.d`), the deprecated IPv4-compatible (`::a.b.c.d`), the
/// NAT64 well-known prefix (`64:ff9b::/96`, reaching the v4 target through a
/// NAT64 gateway), and 6to4 (`2002:AABB:CCDD::/48`).
fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = v6.to_ipv4() {
        return Some(v4);
    }

    let s = v6.segments();
    let octets = v6.octets();

    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
        return Some(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ));
    }

    if s[0] == 0x2002 {
        return Some(Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]));
    }

    None
}

/// Non-public IPv6 ranges without an embedded v4 target: unique local
/// (`fc00::/7`), link-local (`fe80::/10`), the deprecated site-local
/// (`fec0::/10`), and the local-use NAT64 prefix (`64:ff9b:1::/48`), whose
/// v4 embedding position depends on the operator's prefix length — so the
/// whole range is refused.
fn is_private_v6(v6: Ipv6Addr) -> bool {
    let s = v6.segments();

    (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1)
}

/// Check whether an IP address is private/loopback/link-local/unspecified,
/// directly or through an IPv6 form that embeds such an IPv4 address.
pub(super) fn is_private_ip(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }

    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => embedded_v4(v6).map_or_else(|| is_private_v6(v6), is_private_v4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_rejects_loopback() {
        let err = validate_url("http://127.0.0.1/foo").unwrap_err();
        assert!(err.contains("blocked"), "unexpected: {err}");
    }

    #[test]
    fn validate_url_rejects_private_10() {
        let err = validate_url("http://10.0.0.1/foo").unwrap_err();
        assert!(err.contains("blocked"), "unexpected: {err}");
    }

    #[test]
    fn validate_url_rejects_private_192() {
        let err = validate_url("http://192.168.1.1/foo").unwrap_err();
        assert!(err.contains("blocked"), "unexpected: {err}");
    }

    #[test]
    fn validate_url_rejects_link_local() {
        let err = validate_url("http://169.254.0.1/foo").unwrap_err();
        assert!(err.contains("blocked"), "unexpected: {err}");
    }

    // The Lua-visible error must NOT leak the resolved IP or any information
    // about which private-network class was hit. Operators still get the
    // full detail via `tracing::warn!` in `blocked`.
    #[test]
    fn ssrf_error_message_does_not_leak_ip() {
        for url in [
            "http://127.0.0.1/foo",
            "http://10.0.0.1/foo",
            "http://192.168.1.1/foo",
            "http://169.254.0.1/foo",
            "http://172.16.0.1/foo",
        ] {
            let err = validate_url(url).unwrap_err();

            // No IP literal.
            assert!(
                !err.contains("127.0.0.1")
                    && !err.contains("10.0.0.1")
                    && !err.contains("192.168.1.1")
                    && !err.contains("169.254.0.1")
                    && !err.contains("172.16.0.1"),
                "error leaks IP for {url}: {err}"
            );

            // No class hint ("private network", "loopback", etc.) either —
            // those also narrow the search space for an attacker.
            let lc = err.to_ascii_lowercase();
            assert!(
                !lc.contains("private network")
                    && !lc.contains("loopback")
                    && !lc.contains("link-local")
                    && !lc.contains("unspecified"),
                "error leaks address class for {url}: {err}"
            );
        }
    }

    #[test]
    fn validate_url_rejects_unsupported_scheme() {
        let err = validate_url("ftp://example.com/foo").unwrap_err();
        assert!(err.contains("unsupported scheme"), "unexpected: {err}");
    }

    #[test]
    fn validate_url_allows_public() {
        let (host, addr) = validate_url("https://93.184.215.14").unwrap();
        assert_eq!(host, "93.184.215.14");
        assert!(!is_private_ip(addr.ip()));
    }

    #[test]
    fn validate_url_returns_hostname_and_addr() {
        let (host, addr) = validate_url("https://93.184.215.14:443/path").unwrap();
        assert_eq!(host, "93.184.215.14");
        assert_eq!(addr.port(), 443);
        assert!(!is_private_ip(addr.ip()));
    }

    #[test]
    fn is_private_ip_detects_loopback() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_detects_rfc1918() {
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_allows_public() {
        assert!(!is_private_ip("93.184.215.14".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_detects_ipv6_mapped_ipv4() {
        // ::ffff:127.0.0.1 — loopback via IPv6-mapped
        assert!(is_private_ip("::ffff:127.0.0.1".parse().unwrap()));
        // ::ffff:10.0.0.1 — RFC1918 via IPv6-mapped
        assert!(is_private_ip("::ffff:10.0.0.1".parse().unwrap()));
        // ::ffff:192.168.1.1 — RFC1918 via IPv6-mapped
        assert!(is_private_ip("::ffff:192.168.1.1".parse().unwrap()));
        // ::ffff:169.254.0.1 — link-local via IPv6-mapped
        assert!(is_private_ip("::ffff:169.254.0.1".parse().unwrap()));
        // ::ffff:0.0.0.0 — unspecified via IPv6-mapped
        assert!(is_private_ip("::ffff:0.0.0.0".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_detects_unspecified() {
        assert!(is_private_ip("0.0.0.0".parse().unwrap()));
        assert!(is_private_ip("::".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_allows_public_ipv6_mapped() {
        // ::ffff:93.184.215.14 — public via IPv6-mapped
        assert!(!is_private_ip("::ffff:93.184.215.14".parse().unwrap()));
    }

    /// Regression: CGNAT (100.64.0.0/10 — Tailscale/fly.io internal),
    /// 192.0.0.0/24, 198.18.0.0/15, and the deprecated IPv4-compatible
    /// IPv6 form were not blocked.
    #[test]
    fn is_private_ip_detects_special_use_ranges() {
        assert!(is_private_ip("100.64.0.1".parse().unwrap()));
        assert!(is_private_ip("100.101.102.103".parse().unwrap()));
        assert!(is_private_ip("100.127.255.255".parse().unwrap()));
        assert!(is_private_ip("192.0.0.1".parse().unwrap()));
        assert!(is_private_ip("198.18.0.1".parse().unwrap()));
        assert!(is_private_ip("198.19.255.255".parse().unwrap()));
        // IPv4-compatible IPv6 embedding of an RFC1918 address
        assert!(is_private_ip("::192.168.1.1".parse().unwrap()));
        // Boundary neighbors stay public
        assert!(!is_private_ip("100.63.255.255".parse().unwrap()));
        assert!(!is_private_ip("100.128.0.0".parse().unwrap()));
        assert!(!is_private_ip("198.17.255.255".parse().unwrap()));
        assert!(!is_private_ip("198.20.0.0".parse().unwrap()));
    }

    /// Regression: only `0.0.0.0` itself was refused from the "this network"
    /// block; the rest of 0.0.0.0/8 passed.
    #[test]
    fn is_private_ip_detects_this_network_block() {
        assert!(is_private_ip("0.1.2.3".parse().unwrap()));
        assert!(is_private_ip("0.255.255.255".parse().unwrap()));
        assert!(is_private_ip("::ffff:0.1.2.3".parse().unwrap()));
        assert!(!is_private_ip("1.0.0.1".parse().unwrap()));
    }

    /// Regression: the NAT64 well-known prefix embeds an IPv4 target the
    /// gateway connects to — `64:ff9b::a9fe:a9fe` reached the cloud metadata
    /// service (169.254.169.254) and `64:ff9b::a00:1` reached 10.0.0.1 on
    /// NAT64/DNS64 hosts. The embedded address is re-checked like the mapped
    /// forms; a public embedded address stays allowed, and the local-use
    /// NAT64 prefix is refused as a whole.
    #[test]
    fn is_private_ip_checks_the_nat64_embedded_address() {
        assert!(is_private_ip("64:ff9b::a9fe:a9fe".parse().unwrap()));
        assert!(is_private_ip("64:ff9b::a00:1".parse().unwrap()));
        assert!(is_private_ip("64:ff9b::7f00:1".parse().unwrap()));
        assert!(!is_private_ip("64:ff9b::808:808".parse().unwrap()));
        assert!(is_private_ip("64:ff9b:1::808:808".parse().unwrap()));
    }

    /// 6to4 embeds the v4 address right after the `2002::/16` prefix.
    #[test]
    fn is_private_ip_checks_the_6to4_embedded_address() {
        assert!(is_private_ip("2002:a00:1::1".parse().unwrap()));
        assert!(is_private_ip("2002:a9fe:a9fe::".parse().unwrap()));
        assert!(!is_private_ip("2002:808:808::1".parse().unwrap()));
    }

    /// Regression: the deprecated site-local range `fec0::/10` was not
    /// matched (the link-local mask only covers `fe80::/10`).
    #[test]
    fn is_private_ip_detects_site_local_v6() {
        assert!(is_private_ip("fec0::1".parse().unwrap()));
        assert!(is_private_ip("feff:ffff::1".parse().unwrap()));
        assert!(is_private_ip("fe80::1".parse().unwrap()));
        assert!(is_private_ip("fd00::1".parse().unwrap()));
        assert!(!is_private_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
