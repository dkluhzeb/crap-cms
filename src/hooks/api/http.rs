//! `crap.http` namespace — outbound HTTP via reqwest (blocking, safe in spawn_blocking context).

use std::{
    io::Read as _,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    result::Result as StdResult,
    time::Duration,
};

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};
use reqwest::{Method, blocking::Client, redirect};
use tracing::{debug, warn};
use url::Url;

const MAX_REDIRECTS: u8 = 10;
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

/// Register `crap.http` — outbound HTTP via reqwest (blocking, safe in spawn_blocking context).
pub(super) fn register_http(
    lua: &Lua,
    crap: &Table,
    allow_private_networks: bool,
    max_response_bytes: u64,
) -> Result<()> {
    if !allow_private_networks {
        debug!("crap.http: private network blocking enabled with DNS pinning");
    }

    let t = lua.create_table()?;

    t.set(
        "request",
        lua.create_function(move |lua, opts: Table| {
            http_request(lua, &opts, allow_private_networks, max_response_bytes)
        })?,
    )?;

    crap.set("http", t)?;

    Ok(())
}

/// Execute an HTTP request with SSRF protection and redirect following.
fn http_request(
    lua: &Lua,
    opts: &Table,
    allow_private_networks: bool,
    max_response_bytes: u64,
) -> LuaResult<Table> {
    let r = parse_request_opts(opts)?;

    let mut current_url = r.url;
    let mut current_client =
        resolve_and_build_client(&current_url, allow_private_networks, r.timeout)?;
    let mut redirects: u8 = 0;

    loop {
        let mut req = current_client.request(r.method.clone(), &current_url);

        for (k, v) in &r.headers {
            req = req.header(k.as_str(), v.as_str());
        }

        if redirects == 0
            && let Some(ref b) = r.body
        {
            req = req.body(b.clone());
        }

        let resp = req
            .send()
            .map_err(|e| RuntimeError(format!("HTTP transport error: {e}")))?;

        if resp.status().is_redirection() {
            let next = follow_redirect(
                &current_url,
                &resp,
                &mut redirects,
                allow_private_networks,
                r.timeout,
            )?;
            current_url = next.0;
            current_client = next.1;

            continue;
        }

        return build_response_table(lua, resp, max_response_bytes);
    }
}

/// Parsed HTTP request options from Lua.
struct RequestOpts {
    method: Method,
    url: String,
    timeout: Duration,
    body: Option<String>,
    headers: Vec<(String, String)>,
}

/// Parse request options from the Lua table.
fn parse_request_opts(opts: &Table) -> LuaResult<RequestOpts> {
    let url: String = opts.get("url")?;
    let method_str: String = opts
        .get::<Option<String>>("method")?
        .unwrap_or_else(|| "GET".to_string())
        .to_uppercase();

    if !ALLOWED_METHODS.contains(&method_str.as_str()) {
        return Err(RuntimeError(format!(
            "unsupported HTTP method: {method_str}"
        )));
    }

    let method: Method = method_str
        .parse()
        .map_err(|e| RuntimeError(format!("invalid HTTP method: {e}")))?;

    let timeout = Duration::from_secs(opts.get::<Option<u64>>("timeout")?.unwrap_or(30));
    let body: Option<String> = opts.get("body")?;

    let headers = opts
        .get::<Table>("headers")
        .map(|h| h.pairs::<String, String>().collect::<LuaResult<Vec<_>>>())
        .unwrap_or(Ok(Vec::new()))?;

    Ok(RequestOpts {
        method,
        url,
        timeout,
        body,
        headers,
    })
}

/// Resolve DNS and build a pinned HTTP client (or unpinned if private networks allowed).
fn resolve_and_build_client(
    url: &str,
    allow_private_networks: bool,
    timeout: Duration,
) -> LuaResult<Client> {
    let pin = if !allow_private_networks {
        let (host, addr) = validate_url(url).map_err(RuntimeError)?;
        Some((host, addr))
    } else {
        None
    };

    build_client(pin.as_ref().map(|(h, a)| (h.as_str(), *a)), timeout).map_err(RuntimeError)
}

/// Handle a redirect: validate Location, re-resolve DNS, return new (url, client).
fn follow_redirect(
    current_url: &str,
    resp: &reqwest::blocking::Response,
    redirects: &mut u8,
    allow_private_networks: bool,
    timeout: Duration,
) -> LuaResult<(String, Client)> {
    *redirects += 1;
    if *redirects > MAX_REDIRECTS {
        return Err(RuntimeError("too many redirects (max 10)".to_string()));
    }

    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| RuntimeError("redirect without Location header".to_string()))?;

    let next_url = Url::parse(current_url)
        .and_then(|base| base.join(location))
        .map_err(|e| RuntimeError(format!("invalid redirect URL: {e}")))?
        .to_string();

    let client = resolve_and_build_client(&next_url, allow_private_networks, timeout)?;

    Ok((next_url, client))
}

/// Build a Lua response table from an HTTP response.
fn build_response_table(
    lua: &Lua,
    resp: reqwest::blocking::Response,
    max_bytes: u64,
) -> LuaResult<Table> {
    let result = lua.create_table()?;
    result.set("status", resp.status().as_u16() as i64)?;

    let headers_out = lua.create_table()?;

    for (name, val) in resp.headers().iter() {
        if let Ok(v) = val.to_str() {
            headers_out.set(name.as_str(), v)?;
        }
    }

    result.set("headers", headers_out)?;

    let mut body_buf = String::new();

    resp.take(max_bytes)
        .read_to_string(&mut body_buf)
        .map_err(|e| RuntimeError(format!("failed to read response body: {e}")))?;

    result.set("body", body_buf)?;

    Ok(result)
}

/// Resolve and validate a URL against SSRF policy.
/// Returns `(hostname, SocketAddr)` — caller pins via `ClientBuilder::resolve()`.
fn validate_url(url_str: &str) -> StdResult<(String, SocketAddr), String> {
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
    for &addr in &addrs {
        if is_private_ip(addr.ip()) {
            continue;
        }

        return Ok((host, addr));
    }

    // All addresses were private. Log the concrete reason for operators,
    // but return a redacted error to the Lua caller — the caller could be
    // attacker-controlled and would otherwise enumerate internal IP
    // topology from these messages (see SEC-C).
    if let Some(addr) = addrs.first() {
        let ip = addr.ip();
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

        return Err(
            "Target resolves to a blocked address; see server logs for details".to_string(),
        );
    }

    Err("DNS resolution returned no addresses".to_string())
}

/// Check whether an IP address is private/loopback/link-local/unspecified.
fn is_private_ip(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }

    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            // Check IPv6-mapped IPv4 (::ffff:x.x.x.x) — extract the inner v4 and re-check
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return mapped.is_loopback()
                    || mapped.is_unspecified()
                    || mapped.is_private()
                    || mapped.is_link_local();
            }

            let segments = v6.segments();

            // fc00::/7 (unique local) or fe80::/10 (link-local)
            (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Build a reqwest blocking client with optional DNS pinning.
fn build_client(pin: Option<(&str, SocketAddr)>, timeout: Duration) -> StdResult<Client, String> {
    let mut builder = Client::builder()
        .timeout(timeout)
        .redirect(redirect::Policy::none());

    if let Some((host, addr)) = pin {
        builder = builder.resolve(host, addr);
    }

    builder
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
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

    // SEC-C regression: the Lua-visible error must NOT leak the resolved IP
    // or any information about which private-network class was hit. Operators
    // still get the full detail via `tracing::warn!` in validate_url.
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
    fn build_client_no_pin() {
        let client = build_client(None, std::time::Duration::from_secs(5));
        assert!(client.is_ok());
    }

    #[test]
    fn build_client_with_pin() {
        let addr: SocketAddr = "93.184.215.14:443".parse().unwrap();
        let client = build_client(
            Some(("example.com", addr)),
            std::time::Duration::from_secs(5),
        );
        assert!(client.is_ok());
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
}
