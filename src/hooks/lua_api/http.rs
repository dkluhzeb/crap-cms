//! `crap.http` namespace — outbound HTTP via reqwest (blocking, safe in `spawn_blocking` context).

mod body;
mod ssrf;

use std::{
    collections::HashMap, io::Read as _, net::SocketAddr, result::Result as StdResult,
    time::Duration,
};

use anyhow::Result;
use mlua::{
    Error as LuaError, Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table,
    Value,
};
use reqwest::{
    Error as ReqwestError, Method, StatusCode,
    blocking::{Client, Response},
    redirect,
};
use serde::Deserialize;
use tracing::debug;
use url::Url;

use crate::{
    hooks::{
        lifecycle::{check_execution_deadline, execution_time_left},
        lua_api::to_lua_value,
    },
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

use self::{body::LuaBody, ssrf::validate_url};

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
    /// Request body — any Lua string, binary data included.
    #[lua(ty = "string", optional)]
    pub(crate) body: Option<LuaBody>,
    /// Request timeout in seconds; fractional values allowed
    /// (e.g. `0.5` = 500 ms). Default: `30`.
    pub(crate) timeout: Option<f64>,
}

impl FromLua for HttpRequest {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        lua.from_value(value)
    }
}

/// Response returned by `crap.http.request(opts)`. The same Rust struct
/// drives the `types/crap.lua` annotation and the runtime table
/// ([`HttpResponse::into_lua`]).
#[derive(LuaAnnotation)]
#[lua(class = "crap.HttpResponse")]
pub(crate) struct HttpResponse {
    /// HTTP status code.
    pub(crate) status: i64,
    /// Response headers.
    #[lua(ty = "table<string, string>")]
    pub(crate) headers: HashMap<String, String>,
    /// Response body — the bytes as received (a Lua string holds binary
    /// data too).
    #[lua(ty = "string")]
    pub(crate) body: Vec<u8>,
}

impl HttpResponse {
    /// The Lua table handed back to the caller. The body becomes a Lua string
    /// of the raw bytes — not through JSON, which has no byte strings.
    fn into_lua(self, lua: &Lua) -> LuaResult<Table> {
        let tbl = lua.create_table()?;

        tbl.set("status", self.status)?;
        tbl.set("headers", to_lua_value(lua, &self.headers)?)?;
        tbl.set("body", lua.create_string(&self.body)?)?;

        Ok(tbl)
    }
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

    let mut hop = Hop {
        url: r.url.clone(),
        method: r.method.clone(),
        body: r.body.clone(),
    };
    let mut redirects: u8 = 0;

    loop {
        let resp = send_hop(state, lua, &r, &hop)?;

        if !is_followed_redirect(resp.status()) {
            return into_lua_response(lua, resp, state.max_response_bytes);
        }

        hop = next_hop(hop, &resp, &mut redirects)?;
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
    body: Option<Vec<u8>>,
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
        body: opts.body.map(|body| body.0),
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

/// One request of a (possibly redirected) exchange.
struct Hop {
    url: String,
    method: Method,
    body: Option<Vec<u8>>,
}

/// Send one hop of the exchange on a freshly resolved (and, without
/// `allow_private_networks`, pinned) client.
fn send_hop(state: &HttpState, lua: &Lua, r: &RequestOpts, hop: &Hop) -> LuaResult<Response> {
    let timeout = hop_timeout(lua, r.timeout)?;
    let client = resolve_and_build_client(&hop.url, state.allow_private_networks, timeout)?;

    // Sensitive headers (Authorization, Cookie, …) are only replayed to the
    // ORIGINAL origin — a redirect to another scheme, host or port must not
    // receive the caller's credentials (reqwest's own redirect policy draws
    // the same line).
    let keep_credentials = same_origin(&hop.url, &r.url);

    let mut req = client.request(hop.method.clone(), &hop.url);

    for (k, v) in &r.headers {
        if !keep_credentials && is_sensitive_header(k) {
            continue;
        }

        req = req.header(k.as_str(), v.as_str());
    }

    if let Some(ref b) = hop.body {
        req = req.body(b.clone());
    }

    req.send().map_err(|e| transport_error(lua, &e))
}

/// The timeout of the next hop: the caller's `timeout`, bounded by what is
/// left of the VM's job deadline. A job past its deadline stops before its
/// next request (or redirect hop); one still inside it cannot overrun it
/// with a request already in flight either.
fn hop_timeout(lua: &Lua, requested: Duration) -> LuaResult<Duration> {
    let left = execution_time_left(lua)?;

    Ok(left.map_or(requested, |left| requested.min(left)))
}

/// A failed send: the job deadline's error when the hop was cut short by
/// it (the timeout the hop ran under was the deadline's), the transport
/// error otherwise.
fn transport_error(lua: &Lua, e: &ReqwestError) -> LuaError {
    if let Err(deadline) = check_execution_deadline(lua) {
        return deadline;
    }

    RuntimeError(format!("HTTP transport error: {e}"))
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

/// The hop a redirect response leads to: the resolved `Location`, with
/// standard method semantics — 303 (and 301/302 for non-GET/HEAD) switch to
/// GET and drop the body; 307/308 preserve method + body. The target is
/// vetted against the SSRF policy when its hop is sent.
fn next_hop(hop: Hop, resp: &Response, redirects: &mut u8) -> LuaResult<Hop> {
    *redirects += 1;
    if *redirects > MAX_REDIRECTS {
        return Err(RuntimeError("too many redirects (max 10)".to_string()));
    }

    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| RuntimeError("redirect without Location header".to_string()))?;

    let url = Url::parse(&hop.url)
        .and_then(|base| base.join(location))
        .map_err(|e| RuntimeError(format!("invalid redirect URL: {e}")))?
        .to_string();

    let rewrite_to_get = match resp.status().as_u16() {
        303 => true,
        301 | 302 => hop.method != Method::GET && hop.method != Method::HEAD,
        _ => false,
    };

    if rewrite_to_get {
        return Ok(Hop {
            url,
            method: Method::GET,
            body: None,
        });
    }

    Ok(Hop { url, ..hop })
}

/// Whether a response is a redirect the client follows: 301, 302, 303,
/// 307 and 308. The other 3xx statuses are final answers handed back to the
/// caller — `304 Not Modified` answers a conditional request (and carries no
/// `Location`), `300 Multiple Choices` asks the caller to pick one.
fn is_followed_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Whether two URLs share an origin — scheme, host and port (the scheme's
/// default when omitted). An unparseable URL shares no origin.
fn same_origin(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (Url::parse(a), Url::parse(b)) else {
        return false;
    };

    a.scheme() == b.scheme()
        && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Headers that carry credentials — never replayed to a different origin on
/// redirect (same list reqwest's redirect policy scrubs).
fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "cookie2" | "proxy-authorization" | "www-authenticate"
    )
}

/// Convert the final (non-redirect) response into the Lua response table.
fn into_lua_response(lua: &Lua, resp: Response, max_bytes: u64) -> LuaResult<Table> {
    build_response_struct(resp, max_bytes)?.into_lua(lua)
}

/// Build a `HttpResponse` from a `reqwest` response.
fn build_response_struct(resp: Response, max_bytes: u64) -> LuaResult<HttpResponse> {
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

    Ok(HttpResponse {
        status,
        headers,
        body: body_bytes,
    })
}

/// Build a reqwest blocking client with optional DNS pinning.
///
/// `no_proxy` is not optional: the pin below is the whole SSRF control, and
/// reqwest otherwise picks up `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` from
/// the environment and hands the *hostname* to the proxy, which resolves it
/// itself — routing straight past the address this build vetted.
fn build_client(pin: Option<(&str, SocketAddr)>, timeout: Duration) -> StdResult<Client, String> {
    let mut builder = Client::builder()
        .timeout(timeout)
        .no_proxy()
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

    /// Guard: the vetted-address pin is only a control while the client
    /// refuses the environment's proxy — a proxied request sends the
    /// *hostname* and lets the proxy resolve it, landing wherever DNS says
    /// rather than at the address this build checked. A built `reqwest`
    /// client exposes nothing about its proxy configuration, so the builder
    /// chain is pinned in source.
    #[test]
    fn build_client_disables_environment_proxies() {
        let source: String = include_str!("http.rs")
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let chain = source
            .split_once("fn build_client(")
            .expect("build_client exists")
            .1
            .split_once("Client::builder()")
            .expect("build_client builds a client")
            .1
            .split_once(';')
            .expect("the builder chain ends in a statement")
            .0;

        assert!(
            chain.contains(".no_proxy()"),
            "build_client must call .no_proxy(); chain was: {chain}"
        );
    }

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
    fn build_client_no_pin() {
        let client = build_client(None, Duration::from_secs(5));
        assert!(client.is_ok());
    }

    #[test]
    fn build_client_with_pin() {
        let addr: SocketAddr = "93.184.215.14:443".parse().unwrap();
        let client = build_client(Some(("example.com", addr)), Duration::from_secs(5));
        assert!(client.is_ok());
    }

    // ── Scripted local server for redirect / body-limit semantics ──────

    use std::{io::Write as _, net::TcpListener, sync::mpsc, thread, time::Instant};

    use crate::hooks::lifecycle::{ExecutionDeadline, ExecutionDeadlineGuard};

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

    /// Regression: every 3xx was treated as a redirect, so a conditional
    /// request answered `304 Not Modified` (no `Location`) failed with
    /// "redirect without Location header" instead of returning the 304.
    #[test]
    fn not_modified_is_returned_not_followed() {
        let not_modified =
            "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n".to_string();
        let (base, _rx) = scripted_server(vec![not_modified]);
        let lua = lua_with_http(1024);

        let code = format!(
            r#"
            local resp = crap.http.request({{
                url = "{base}/cached",
                headers = {{ ["If-None-Match"] = '"v1"' }},
                timeout = 5,
            }})
            return resp.status
            "#
        );
        let status: i64 = lua.load(&code).eval().unwrap();

        assert_eq!(status, 304);
    }

    #[test]
    fn only_redirect_statuses_are_followed() {
        for code in [301, 302, 303, 307, 308] {
            assert!(
                is_followed_redirect(StatusCode::from_u16(code).unwrap()),
                "{code}"
            );
        }

        for code in [300, 304, 305, 200, 404] {
            assert!(
                !is_followed_redirect(StatusCode::from_u16(code).unwrap()),
                "{code}"
            );
        }
    }

    /// Regression: the redirect credential scrub compared hosts only, so a
    /// redirect to the same host on another port or scheme (a different
    /// service, possibly over cleartext) received the caller's credentials.
    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        assert!(same_origin(
            "https://api.x.com/a",
            "https://API.x.com:443/b"
        ));
        assert!(same_origin("http://api.x.com/a", "http://api.x.com:80/b"));
        assert!(!same_origin("https://api.x.com/", "http://api.x.com/"));
        assert!(!same_origin(
            "https://api.x.com/",
            "https://api.x.com:8443/"
        ));
        assert!(!same_origin("https://api.x.com/", "https://other.x.com/"));
        assert!(!same_origin("not a url", "not a url"));
    }

    /// End-to-end: a redirect to the same host on another port drops the
    /// `Authorization` header; a non-sensitive header still travels.
    #[test]
    fn redirect_to_another_port_drops_credentials() {
        let (target, target_rx) = scripted_server(vec![ok_resp("ok")]);
        let hop = format!(
            "HTTP/1.1 302 Found\r\nLocation: {target}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (base, _rx) = scripted_server(vec![hop]);
        let lua = lua_with_http(1024 * 1024);

        let code = format!(
            r#"
            local resp = crap.http.request({{
                url = "{base}/start",
                headers = {{ Authorization = "Bearer s3cret", ["X-Trace"] = "t1" }},
                timeout = 5,
            }})
            return resp.status
            "#
        );
        let status: i64 = lua.load(&code).eval().unwrap();
        assert_eq!(status, 200);

        let replayed = target_rx.recv().unwrap().to_ascii_lowercase();
        assert!(
            !replayed.contains("s3cret"),
            "credentials must not cross to another port:\n{replayed}"
        );
        assert!(replayed.contains("x-trace: t1"), "{replayed}");
    }

    /// Answer one request with its own body, byte for byte.
    fn echo_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];

            let body = loop {
                let n = stream.read(&mut tmp).unwrap();
                assert!(n > 0, "the client closed before sending its request");
                buf.extend_from_slice(&tmp[..n]);

                let Some(pos) = headers_end(&buf) else {
                    continue;
                };

                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                let want = pos + 4 + content_length(&head);

                while buf.len() < want {
                    let n = stream.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                }

                break buf[pos + 4..want].to_vec();
            };

            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });

        format!("http://{addr}")
    }

    /// Regression: request and response bodies went through Rust `String`s,
    /// so any non-UTF-8 byte failed the call — a custom storage backend could
    /// not `PUT` or `GET` an image. Both directions now carry raw bytes.
    #[test]
    fn binary_bodies_round_trip() {
        let base = echo_server();
        let lua = lua_with_http(1024);

        let code = format!(
            r#"
            local sent = "\0\255\128png"
            local resp = crap.http.request({{
                url = "{base}/echo",
                method = "PUT",
                body = sent,
                timeout = 5,
            }})
            return resp.body == sent, #resp.body
            "#
        );
        let (same, len): (bool, i64) = lua.load(&code).eval().unwrap();

        assert!(same, "the binary body must round-trip unchanged");
        assert_eq!(len, 6);
    }

    /// Accept connections and never answer — a request to it hangs until its
    /// timeout.
    fn silent_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        thread::spawn(move || {
            let mut held = Vec::new();

            while let Ok((stream, _)) = listener.accept() {
                held.push(stream);
            }
        });

        format!("http://{addr}")
    }

    /// Regression: the job deadline was checked only before a request, which
    /// then ran for its full `timeout` — a job with a short timeout doing a
    /// long request overran its deadline by up to that timeout. The hop's
    /// timeout is now bounded by the time the deadline leaves, and the
    /// failure reports the deadline.
    #[test]
    fn a_request_in_flight_stops_at_the_job_deadline() {
        let base = silent_server();
        let lua = lua_with_http(1024);
        let _deadline = ExecutionDeadlineGuard::install(&lua, ExecutionDeadline::new(1));

        let started = Instant::now();
        let err = lua
            .load(format!(
                r#"return crap.http.request({{ url = "{base}/hang", timeout = 60 }})"#
            ))
            .exec()
            .expect_err("the request must stop at the deadline");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the request ran past the deadline: {:?}",
            started.elapsed()
        );
        assert!(
            err.to_string().contains("exceeded its timeout"),
            "unexpected error: {err}"
        );
    }
}
