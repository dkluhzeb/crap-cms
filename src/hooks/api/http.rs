//! `crap.http` namespace — outbound HTTP via ureq (blocking, safe in spawn_blocking context).

use anyhow::Result;
use mlua::{Lua, Table};

/// Register `crap.http` — outbound HTTP via ureq (blocking, safe in spawn_blocking context).
pub(super) fn register_http(lua: &Lua, crap: &Table) -> Result<()> {
    let http_table = lua.create_table()?;
    let http_request_fn = lua.create_function(|lua, opts: Table| -> mlua::Result<Table> {
        let url: String = opts.get("url")?;
        let method: String = opts.get::<Option<String>>("method")?
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();
        let timeout: u64 = opts.get::<Option<u64>>("timeout")?.unwrap_or(30);
        let body: Option<String> = opts.get("body")?;

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(timeout)))
            .http_status_as_error(false)
            .build()
            .new_agent();

        // Collect headers
        let headers: Vec<(String, String)> = if let Ok(headers_tbl) = opts.get::<Table>("headers") {
            headers_tbl.pairs::<String, String>()
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
            _ => return Err(mlua::Error::RuntimeError(
                format!("unsupported HTTP method: {}", method)
            )),
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
                let body_str = resp.body_mut().read_to_string()
                    .map_err(|e| mlua::Error::RuntimeError(
                        format!("failed to read response body: {}", e)
                    ))?;
                result.set("body", body_str)?;
            }
            Err(e) => {
                return Err(mlua::Error::RuntimeError(
                    format!("HTTP transport error: {}", e)
                ));
            }
        }

        Ok(result)
    })?;
    http_table.set("request", http_request_fn)?;
    crap.set("http", http_table)?;
    Ok(())
}
