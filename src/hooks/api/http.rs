//! `crap.http` namespace — outbound HTTP via ureq (blocking, safe in spawn_blocking context).

use std::net::ToSocketAddrs;

use anyhow::Result;
use mlua::{Lua, Table};

/// Register `crap.http` — outbound HTTP via ureq (blocking, safe in spawn_blocking context).
pub(super) fn register_http(
    lua: &Lua,
    crap: &Table,
    allow_private_networks: bool,
    max_response_bytes: u64,
) -> Result<()> {
    let http_table = lua.create_table()?;
    let http_request_fn = lua.create_function(move |lua, opts: Table| -> mlua::Result<Table> {
        let url: String = opts.get("url")?;
        let method: String = opts
            .get::<Option<String>>("method")?
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();
        let timeout: u64 = opts.get::<Option<u64>>("timeout")?.unwrap_or(30);
        let body: Option<String> = opts.get("body")?;

        if !allow_private_networks {
            validate_url(&url).map_err(mlua::Error::RuntimeError)?;
        }

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(timeout)))
            .http_status_as_error(false)
            .build()
            .new_agent();

        // Collect headers
        let headers: Vec<(String, String)> = if let Ok(headers_tbl) = opts.get::<Table>("headers") {
            headers_tbl
                .pairs::<String, String>()
                .collect::<mlua::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        // Build and send — ureq 3 uses typed builders (WithBody vs WithoutBody),
        // so we must split methods that accept a body from those that don't.
        let response = match method.as_str() {
            "GET" | "DELETE" | "HEAD" => {
                let mut req = match method.as_str() {
                    "GET" => agent.get(&url),
                    "DELETE" => agent.delete(&url),
                    "HEAD" => agent.head(&url),
                    _ => unreachable!(),
                };
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                req.call()
            }
            "POST" | "PUT" | "PATCH" => {
                let mut req = match method.as_str() {
                    "POST" => agent.post(&url),
                    "PUT" => agent.put(&url),
                    "PATCH" => agent.patch(&url),
                    _ => unreachable!(),
                };
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                if let Some(ref body_str) = body {
                    req.send(body_str.as_str())
                } else {
                    req.send("")
                }
            }
            _ => {
                return Err(mlua::Error::RuntimeError(format!(
                    "unsupported HTTP method: {}",
                    method
                )));
            }
        };

        let result = lua.create_table()?;
        match response {
            Ok(mut resp) => {
                result.set("status", resp.status().as_u16() as i64)?;
                let headers_out = lua.create_table()?;
                for (name, val) in resp.headers().iter() {
                    if let Ok(v) = val.to_str() {
                        headers_out.set(name.as_str(), v)?;
                    }
                }
                result.set("headers", headers_out)?;
                let body_str = resp
                    .body_mut()
                    .with_config()
                    .limit(max_response_bytes)
                    .read_to_string()
                    .map_err(|e| {
                        mlua::Error::RuntimeError(format!("failed to read response body: {}", e))
                    })?;
                result.set("body", body_str)?;
            }
            Err(e) => {
                return Err(mlua::Error::RuntimeError(format!(
                    "HTTP transport error: {}",
                    e
                )));
            }
        }

        Ok(result)
    })?;
    http_table.set("request", http_request_fn)?;
    crap.set("http", http_table)?;
    Ok(())
}

/// Validate that a URL does not target private/loopback/link-local networks.
fn validate_url(url_str: &str) -> std::result::Result<(), String> {
    let parsed = url::Url::parse(url_str).map_err(|e| format!("invalid URL: {e}"))?;

    // Only allow http/https
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("unsupported scheme: {s}")),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;

    // Resolve hostname and check all addresses
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed: {e}"))?;

    for addr in addrs {
        let ip = addr.ip();
        if ip.is_loopback() || ip.is_unspecified() {
            return Err(format!("requests to {ip} are blocked"));
        }
        if let std::net::IpAddr::V4(v4) = ip
            && (v4.is_private() || v4.is_link_local())
        {
            return Err(format!("requests to private network {ip} are blocked"));
        }
        if let std::net::IpAddr::V6(v6) = ip {
            if v6.is_loopback() {
                return Err(format!("requests to {ip} are blocked"));
            }
            let segments = v6.segments();
            // fc00::/7 (unique local) and fe80::/10 (link-local)
            if (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80 {
                return Err(format!("requests to private network {ip} are blocked"));
            }
        }
    }
    Ok(())
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
        // Use a known public IP directly to avoid DNS resolution failures in sandboxed environments
        assert!(validate_url("https://93.184.215.14").is_ok());
    }
}
