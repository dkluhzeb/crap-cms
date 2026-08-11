//! `crap.http` namespace — outbound HTTP via reqwest (blocking, safe in `spawn_blocking` context).

use std::{
    io::Read as _,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    result::Result as StdResult,
    time::Duration,
};

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use reqwest::{Method, blocking::Client, redirect};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use url::Url;

use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

const MAX_REDIRECTS: u8 = 10;
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

/// Options table for `crap.http.request`. Unknown keys are rejected.
#[derive(Deserialize, LuaAnnotation)]
#[serde(deny_unknown_fields)]
#[lua(class = "crap.HttpRequest")]
pub(crate) struct HttpRequest {
    /// Request URL.
    pub(crate) url: String,
    /// HTTP method (default: `"GET"`).
    pub(crate) method: Option<String>,
    /// Request headers.
    #[lua(ty = "table<string, string>", optional)]
    pub(crate) headers: Option<HashMap<String, String>>,
    /// Request body.
    pub(crate) body: Option<String>,
    /// Request timeout in seconds; fractional values allowed
    /// (e.g. `0.5` = 500 ms). Default: `30`.
    pub(crate) timeout: Option<f64>,
}

impl FromLua for HttpRequest {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        lua.from_value(value)
    }
}

/// Response returned by `crap.http.request(opts)`. Both `LuaAnnotation`
/// (for `types/crap.lua`) and `Serialize` (for the runtime
/// `lua.to_value(&self)` conversion); the same Rust struct is the
/// single source of truth.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.HttpResponse")]
pub(crate) struct HttpResponse {
    /// HTTP status code.
    pub(crate) status: i64,
    /// Response headers.
    #[lua(ty = "table<string, string>")]
    pub(crate) headers: HashMap<String, String>,
    /// Response body.
    pub(crate) body: String,
}

/// Closure state for the `crap.http.*` namespace — captured once at
/// registration time, threaded into every call.
pub(super) struct HttpState {
    allow_private_networks: bool,
    max_response_bytes: u64,
}

/// Make an outbound HTTP request. Blocking — safe inside `spawn_blocking`
/// contexts (which is where Lua hooks run). DNS-pinned when private
/// networks are disabled in `crap.toml`.
#[lua_fn(path = "crap.http.request", returns = "crap.HttpResponse")]
fn http_request(
    state: &HttpState,
    lua: &Lua,
    #[lua(ty = "crap.HttpRequest", doc = "Request options.")] opts: HttpRequest,
) -> LuaResult<Table> {
    let r = parse_request_opts(opts)?;

    let original_host = host_of(&r.url);
    let mut current_url = r.url;
    let mut current_client =
        resolve_and_build_client(&current_url, state.allow_private_networks, r.timeout)?;
    let mut current_method = r.method.clone();
    let mut current_body = r.body.clone();
    let mut redirects: u8 = 0;

    loop {
        let mut req = current_client.request(current_method.clone(), &current_url);

        // Sensitive headers (Authorization, Cookie, …) are only replayed to
        // the ORIGINAL host — a redirect to another host must not receive
        // the caller's credentials (matches reqwest's own redirect policy).
        let same_host = host_of(&current_url) == original_host;
        for (k, v) in &r.headers {
            if !same_host && is_sensitive_header(k) {
                continue;
            }

            req = req.header(k.as_str(), v.as_str());
        }

        if let Some(ref b) = current_body {
            req = req.body(b.clone());
        }

        let resp = req
            .send()
            .map_err(|e| RuntimeError(format!("HTTP transport error: {e}")))?;

        if resp.status().is_redirection() {
            let status = resp.status().as_u16();
            let next = follow_redirect(
                &current_url,
                &resp,
                &mut redirects,
                state.allow_private_networks,
                r.timeout,
            )?;
            current_url = next.0;
            current_client = next.1;

            // Standard redirect semantics: 303 (and 301/302 for non-GET/HEAD)
            // switch to GET and drop the body; 307/308 preserve method + body.
            match status {
                303 => {
                    current_method = Method::GET;
                    current_body = None;
                }
                301 | 302 if current_method != Method::GET && current_method != Method::HEAD => {
                    current_method = Method::GET;
                    current_body = None;
                }
                _ => {}
            }

            continue;
        }

        let response = build_response_struct(resp, state.max_response_bytes)?;
        let value = lua.to_value(&response)?;
        let Value::Table(tbl) = value else {
            return Err(RuntimeError(
                "lua.to_value did not produce a table for HttpResponse".into(),
            ));
        };
        return Ok(tbl);
    }
}

lua_table! {
    name: crap_http,
    path: "crap.http",
    state: HttpState,
    header: "Outbound HTTP client (blocking, runs inside spawn_blocking context).",
    fns: [http_request],
}

/// Register `crap.http` — outbound HTTP via reqwest. Parent `crap` table
/// must already be in globals (`register_api` sets it up-front).
pub(super) fn register_http(
    lua: &Lua,
    allow_private_networks: bool,
    max_response_bytes: u64,
) -> Result<()> {
    if !allow_private_networks {
        debug!("crap.http: private network blocking enabled with DNS pinning");
    }
    register_crap_http(
        lua,
        HttpState {
            allow_private_networks,
            max_response_bytes,
        },
    )?;
    Ok(())
}

/// Parsed HTTP request options from Lua.
struct RequestOpts {
    method: Method,
    url: String,
    timeout: Duration,
    body: Option<String>,
    headers: Vec<(String, String)>,
}

/// Parse request options from the typed `HttpRequest`.
fn parse_request_opts(opts: HttpRequest) -> LuaResult<RequestOpts> {
    let method_str = opts
        .method
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

    let timeout = parse_timeout(opts.timeout)?;
    let headers = opts
        .headers
        .map(|h| h.into_iter().collect())
        .unwrap_or_default();

    Ok(RequestOpts {
        method,
        url: opts.url,
        timeout,
        body: opts.body,
        headers,
    })
}

/// Convert the optional `timeout` seconds value (fractional allowed) into a
/// `Duration`. Zero, negative, NaN, and non-finite values are hard errors.
fn parse_timeout(timeout: Option<f64>) -> LuaResult<Duration> {
    let secs = timeout.unwrap_or(30.0);

    if secs.is_nan() || secs <= 0.0 {
        return Err(RuntimeError(format!(
            "invalid timeout: must be a positive number of seconds, got {secs}"
        )));
    }

    Duration::try_from_secs_f64(secs)
        .map_err(|e| RuntimeError(format!("invalid timeout {secs}: {e}")))
}

/// Resolve DNS and build a pinned HTTP client (or unpinned if private networks allowed).
fn resolve_and_build_client(
    url: &str,
    allow_private_networks: bool,
    timeout: Duration,
) -> LuaResult<Client> {
    let pin = if allow_private_networks {
        None
    } else {
        let (host, addr) = validate_url(url).map_err(RuntimeError)?;
        Some((host, addr))
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

/// Lowercased host component of a URL, if parseable.
fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
}

/// Headers that carry credentials — never replayed to a different host on
/// redirect (same list reqwest's redirect policy scrubs).
fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "cookie2" | "proxy-authorization" | "www-authenticate"
    )
}

/// Build a `HttpResponse` from a `reqwest` response.
fn build_response_struct(
    resp: reqwest::blocking::Response,
    max_bytes: u64,
) -> LuaResult<HttpResponse> {
    let status = i64::from(resp.status().as_u16());

    // Duplicate headers (multiple Set-Cookie, Vary, …) are comma-joined
    // rather than last-wins-dropped; non-UTF-8 values are skipped.
    let mut headers: HashMap<String, String> = HashMap::new();
    for (name, val) in resp.headers() {
        let Ok(v) = val.to_str() else {
            continue;
        };

        headers
            .entry(name.as_str().to_string())
            .and_modify(|existing| {
                existing.push_str(", ");
                existing.push_str(v);
            })
            .or_insert_with(|| v.to_string());
    }

    // Read one byte past the cap so an over-limit body is a hard error —
    // silent truncation handed back corrupted data (truncated JSON parsed
    // downstream, or a UTF-8 error when the cut landed mid-character).
    let mut body_bytes = Vec::new();
    resp.take(max_bytes.saturating_add(1))
        .read_to_end(&mut body_bytes)
        .map_err(|e| RuntimeError(format!("failed to read response body: {e}")))?;

    if body_bytes.len() as u64 > max_bytes {
        return Err(RuntimeError(format!(
            "response body exceeds max_response_bytes ({max_bytes})"
        )));
    }

    let body = String::from_utf8(body_bytes)
        .map_err(|e| RuntimeError(format!("response body is not valid UTF-8: {e}")))?;

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
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

/// Non-public IPv4 ranges beyond loopback/unspecified: RFC 1918 private,
/// link-local, CGNAT (100.64.0.0/10 — Tailscale/fly.io internal networks),
/// IETF protocol assignments (192.0.0.0/24), and benchmarking (198.18.0.0/15).
fn is_private_v4(v4: std::net::Ipv4Addr) -> bool {
    let o = v4.octets();

    v4.is_loopback()
        || v4.is_unspecified()
        || v4.is_private()
        || v4.is_link_local()
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
}

/// Check whether an IP address is private/loopback/link-local/unspecified.
fn is_private_ip(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }

    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => {
            // IPv6-mapped (::ffff:x.x.x.x) and the deprecated IPv4-compatible
            // (::x.x.x.x) forms both embed a v4 address — extract and re-check
            // with the full v4 range list.
            if let Some(v4) = v6.to_ipv4() {
                return is_private_v4(v4);
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
    fn http_request_rejects_unknown_key() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("url", "https://example.com").unwrap();
        tbl.set("timout", 5).unwrap();

        let Err(err) = HttpRequest::from_lua(Value::Table(tbl), &lua) else {
            panic!("unknown key must be rejected");
        };
        let err = err.to_string();
        assert!(err.contains("unknown field `timout`"), "unexpected: {err}");
    }

    #[test]
    fn parse_timeout_defaults_to_30s() {
        assert_eq!(parse_timeout(None).unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_timeout_accepts_fractional_seconds() {
        assert_eq!(
            parse_timeout(Some(0.5)).unwrap(),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn parse_timeout_rejects_zero_negative_and_nan() {
        for bad in [0.0, -1.0, f64::NAN] {
            let err = parse_timeout(Some(bad)).unwrap_err().to_string();
            assert!(err.contains("invalid timeout"), "unexpected: {err}");
        }
    }

    #[test]
    fn parse_timeout_rejects_infinite() {
        let err = parse_timeout(Some(f64::INFINITY)).unwrap_err().to_string();
        assert!(err.contains("invalid timeout"), "unexpected: {err}");
    }

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

    // ── Scripted local server for redirect / body-limit semantics ──────

    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    fn headers_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn content_length(head: &str) -> usize {
        head.lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0)
    }

    /// Serve one scripted response per incoming connection, capturing each
    /// raw request. Returns the base URL and the captured-requests channel.
    fn scripted_server(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];

                loop {
                    let n = stream.read(&mut tmp).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);

                    if let Some(pos) = headers_end(&buf) {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                        let want = pos + 4 + content_length(&head);
                        while buf.len() < want {
                            let n = stream.read(&mut tmp).unwrap();
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                        }
                        break;
                    }
                }

                tx.send(String::from_utf8_lossy(&buf).to_string()).unwrap();
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });

        (format!("http://{addr}"), rx)
    }

    fn lua_with_http(max_response_bytes: u64) -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_http(&lua, true, max_response_bytes).unwrap();
        lua
    }

    fn redirect_resp(status: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn ok_resp(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Regression: the redirect loop replayed the method but dropped the
    /// body on every redirect class — a POST hitting a 307 was replayed
    /// with an empty body (silent data loss).
    #[test]
    fn redirect_307_preserves_method_and_body() {
        let (base, rx) =
            scripted_server(vec![redirect_resp("307 Temporary Redirect"), ok_resp("ok")]);
        let lua = lua_with_http(1024 * 1024);

        let code = format!(
            r#"
            local resp = crap.http.request({{
                url = "{base}/start",
                method = "POST",
                body = "hello-body",
                timeout = 5,
            }})
            return resp.status
            "#
        );
        let status: i64 = lua.load(&code).eval().unwrap();
        assert_eq!(status, 200);

        let _first = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert!(
            second.starts_with("POST /next"),
            "307 must preserve the method, got: {}",
            second.lines().next().unwrap_or("")
        );
        assert!(
            second.contains("hello-body"),
            "307 must preserve the body; replayed request:\n{second}"
        );
    }

    /// The 303 side of the same regression: See Other must convert the
    /// replay to GET and drop the body.
    #[test]
    fn redirect_303_converts_to_get_and_drops_body() {
        let (base, rx) = scripted_server(vec![redirect_resp("303 See Other"), ok_resp("ok")]);
        let lua = lua_with_http(1024 * 1024);

        let code = format!(
            r#"
            local resp = crap.http.request({{
                url = "{base}/start",
                method = "POST",
                body = "hello-body",
                timeout = 5,
            }})
            return resp.status
            "#
        );
        let status: i64 = lua.load(&code).eval().unwrap();
        assert_eq!(status, 200);

        let _first = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert!(
            second.starts_with("GET /next"),
            "303 must convert to GET, got: {}",
            second.lines().next().unwrap_or("")
        );
        assert!(
            !second.contains("hello-body"),
            "303 must drop the body; replayed request:\n{second}"
        );
    }

    /// Regression: a response over `max_response_bytes` was silently
    /// truncated (or errored with a confusing UTF-8 message when the cut
    /// landed mid-character). It must be a hard error; an at-limit
    /// response still passes.
    #[test]
    fn oversized_response_body_is_a_hard_error() {
        let (base, _rx) = scripted_server(vec![ok_resp("0123456789")]);
        let lua = lua_with_http(8);

        let code = format!(
            r#"
            local resp = crap.http.request({{ url = "{base}/big", timeout = 5 }})
            return resp.body
            "#
        );
        let err = lua
            .load(&code)
            .eval::<String>()
            .expect_err("over-limit body must error, not truncate");
        assert!(
            err.to_string().contains("max_response_bytes"),
            "unexpected error: {err}"
        );

        let (base, _rx) = scripted_server(vec![ok_resp("01234567")]);
        let lua = lua_with_http(8);
        let code = format!(
            r#"
            local resp = crap.http.request({{ url = "{base}/fits", timeout = 5 }})
            return resp.body
            "#
        );
        let body: String = lua.load(&code).eval().unwrap();
        assert_eq!(body, "01234567", "at-limit body must pass through intact");
    }
}
