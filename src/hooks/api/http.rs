//! `crap.http` namespace — outbound HTTP via reqwest (blocking, safe in spawn_blocking context).

use std::io::Read as _;
use std::net::{SocketAddr, ToSocketAddrs};

use anyhow::Result;
use mlua::{Lua, Table};
use reqwest::redirect;
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
        tracing::debug!("crap.http: private network blocking enabled with DNS pinning");
    }

    let http_table = lua.create_table()?;
    let http_request_fn = lua.create_function(move |lua, opts: Table| -> mlua::Result<Table> {
        let url: String = opts.get("url")?;
        let method: String = opts
            .get::<Option<String>>("method")?
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();
        let timeout: u64 = opts.get::<Option<u64>>("timeout")?.unwrap_or(30);
        let body: Option<String> = opts.get("body")?;
        let timeout_dur = std::time::Duration::from_secs(timeout);

        // Validate method before any network call
        if !ALLOWED_METHODS.contains(&method.as_str()) {
            return Err(mlua::Error::RuntimeError(format!(
                "unsupported HTTP method: {method}"
            )));
        }
        let method: reqwest::Method = method
            .parse()
            .map_err(|e| mlua::Error::RuntimeError(format!("invalid HTTP method: {e}")))?;

        // Collect headers
        let headers: Vec<(String, String)> = if let Ok(headers_tbl) = opts.get::<Table>("headers") {
            headers_tbl
                .pairs::<String, String>()
                .collect::<mlua::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        // Resolve + pin DNS (or skip when private networks allowed)
        let pin = if !allow_private_networks {
            let (host, addr) = validate_url(&url).map_err(mlua::Error::RuntimeError)?;
            Some((host, addr))
        } else {
            None
        };

        let client = build_client(pin.as_ref().map(|(h, a)| (h.as_str(), *a)), timeout_dur)
            .map_err(mlua::Error::RuntimeError)?;

        // Manual redirect loop — re-validate DNS on each hop
        let mut current_url = url;
        let mut current_client = client;
        let mut redirects: u8 = 0;

        loop {
            let mut req = current_client.request(method.clone(), &current_url);
            for (k, v) in &headers {
                req = req.header(k.as_str(), v.as_str());
            }
            // Only attach body on first request (not on redirect follows)
            if redirects == 0
                && let Some(ref body_str) = body
            {
                req = req.body(body_str.clone());
            }

            let resp = req
                .send()
                .map_err(|e| mlua::Error::RuntimeError(format!("HTTP transport error: {e}")))?;

            // Check for redirect
            if resp.status().is_redirection() {
                redirects += 1;
                if redirects > MAX_REDIRECTS {
                    return Err(mlua::Error::RuntimeError(
                        "too many redirects (max 10)".to_string(),
                    ));
                }
                let location = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        mlua::Error::RuntimeError("redirect without Location header".to_string())
                    })?
                    .to_string();

                // Resolve relative redirect
                let next_url = Url::parse(&current_url)
                    .and_then(|base| base.join(&location))
                    .map_err(|e| mlua::Error::RuntimeError(format!("invalid redirect URL: {e}")))?
                    .to_string();

                // Re-validate + re-pin DNS for the new URL
                let next_pin = if !allow_private_networks {
                    let (host, addr) =
                        validate_url(&next_url).map_err(mlua::Error::RuntimeError)?;
                    Some((host, addr))
                } else {
                    None
                };

                current_client = build_client(
                    next_pin.as_ref().map(|(h, a)| (h.as_str(), *a)),
                    timeout_dur,
                )
                .map_err(mlua::Error::RuntimeError)?;
                current_url = next_url;
                continue;
            }

            // Build response table
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
            resp.take(max_response_bytes)
                .read_to_string(&mut body_buf)
                .map_err(|e| {
                    mlua::Error::RuntimeError(format!("failed to read response body: {e}"))
                })?;
            result.set("body", body_buf)?;

            return Ok(result);
        }
    })?;
    http_table.set("request", http_request_fn)?;
    crap.set("http", http_table)?;
    Ok(())
}

/// Resolve and validate a URL against SSRF policy.
/// Returns `(hostname, SocketAddr)` — caller pins via `ClientBuilder::resolve()`.
fn validate_url(url_str: &str) -> std::result::Result<(String, SocketAddr), String> {
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

    // All addresses were private — report the first one in the error
    if let Some(addr) = addrs.first() {
        let ip = addr.ip();
        if ip.is_loopback() || ip.is_unspecified() {
            return Err(format!("requests to {ip} are blocked"));
        }
        return Err(format!("requests to private network {ip} are blocked"));
    }

    Err("DNS resolution returned no addresses".to_string())
}

/// Check whether an IP address is private/loopback/link-local/unspecified.
fn is_private_ip(ip: std::net::IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
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
fn build_client(
    pin: Option<(&str, SocketAddr)>,
    timeout: std::time::Duration,
) -> std::result::Result<reqwest::blocking::Client, String> {
    let mut builder = reqwest::blocking::Client::builder()
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
        assert!(err.contains("private network"), "unexpected: {err}");
    }

    #[test]
    fn validate_url_rejects_private_192() {
        let err = validate_url("http://192.168.1.1/foo").unwrap_err();
        assert!(err.contains("private network"), "unexpected: {err}");
    }

    #[test]
    fn validate_url_rejects_link_local() {
        let err = validate_url("http://169.254.0.1/foo").unwrap_err();
        assert!(err.contains("private network"), "unexpected: {err}");
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
