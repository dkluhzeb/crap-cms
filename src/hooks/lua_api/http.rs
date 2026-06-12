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

    let mut current_url = r.url;
    let mut current_client =
        resolve_and_build_client(&current_url, state.allow_private_networks, r.timeout)?;
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
                state.allow_private_networks,
                r.timeout,
            )?;
            current_url = next.0;
            current_client = next.1;

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

/// Build a `HttpResponse` from a `reqwest` response.
fn build_response_struct(
    resp: reqwest::blocking::Response,
    max_bytes: u64,
) -> LuaResult<HttpResponse> {
    let status = i64::from(resp.status().as_u16());

    let headers = resp
        .headers()
        .iter()
        .filter_map(|(name, val)| {
            val.to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect();

    let mut body = String::new();
    resp.take(max_bytes)
        .read_to_string(&mut body)
        .map_err(|e| RuntimeError(format!("failed to read response body: {e}")))?;

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
}
