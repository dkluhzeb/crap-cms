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

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(timeout))
            .build();

        let mut req = match method.as_str() {
            "GET" => agent.get(&url),
            "POST" => agent.post(&url),
            "PUT" => agent.put(&url),
            "PATCH" => agent.request("PATCH", &url),
            "DELETE" => agent.delete(&url),
            "HEAD" => agent.head(&url),
            _ => return Err(mlua::Error::RuntimeError(
                format!("unsupported HTTP method: {}", method)
            )),
        };

        // Set request headers
        if let Ok(headers_tbl) = opts.get::<Table>("headers") {
            for pair in headers_tbl.pairs::<String, String>() {
                let (k, v) = pair?;
                req = req.set(&k, &v);
            }
        }

        // Send request
        let response = if let Some(body_str) = body {
            req.send_string(&body_str)
        } else {
            req.call()
        };

        let result = lua.create_table()?;
        match response {
            Ok(resp) => {
                result.set("status", resp.status() as i64)?;
                let headers_out = lua.create_table()?;
                for name in resp.headers_names() {
                    if let Some(val) = resp.header(&name) {
                        headers_out.set(name.as_str(), val)?;
                    }
                }
                result.set("headers", headers_out)?;
                let body_str = resp.into_string()
                    .map_err(|e| mlua::Error::RuntimeError(
                        format!("failed to read response body: {}", e)
                    ))?;
                result.set("body", body_str)?;
            }
            Err(ureq::Error::Status(code, resp)) => {
                result.set("status", code as i64)?;
                let headers_out = lua.create_table()?;
                for name in resp.headers_names() {
                    if let Some(val) = resp.header(&name) {
                        headers_out.set(name.as_str(), val)?;
                    }
                }
                result.set("headers", headers_out)?;
                let body_str = resp.into_string().unwrap_or_default();
                result.set("body", body_str)?;
            }
            Err(ureq::Error::Transport(e)) => {
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
