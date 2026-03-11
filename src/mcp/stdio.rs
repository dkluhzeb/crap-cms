//! stdio transport for the MCP server.
//! Reads JSON-RPC messages from stdin, writes responses to stdout.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error};

use super::McpServer;

/// Run the stdio MCP transport. Reads newline-delimited JSON-RPC from stdin,
/// processes each message, and writes responses to stdout.
#[cfg(not(tarpaulin_include))] // requires interactive stdio
pub async fn run_stdio(server: McpServer) {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    // Wrap in Arc so we can cheaply clone for each spawn_blocking call
    let server = Arc::new(server);

    debug!("MCP stdio transport started");

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        debug!("MCP recv: {}", &line[..line.len().min(200)]);

        // Parse JSON-RPC request
        let request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let err_resp = super::protocol::JsonRpcResponse::error(
                    None,
                    super::protocol::PARSE_ERROR,
                    format!("Parse error: {}", e),
                );
                let resp_json = serde_json::to_string(&err_resp).unwrap_or_default();
                let _ = stdout.write_all(resp_json.as_bytes()).await;
                let _ = stdout.write_all(b"\n").await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        // Run handle_message in spawn_blocking — it does DB queries, Lua hooks, and filesystem I/O
        let server_clone = Arc::clone(&server);
        let response =
            match tokio::task::spawn_blocking(move || server_clone.handle_message(request)).await {
                Ok(resp) => resp,
                Err(_) => {
                    error!("MCP spawn_blocking task panicked");
                    super::protocol::JsonRpcResponse::error(
                        None,
                        super::protocol::INTERNAL_ERROR,
                        "Internal error",
                    )
                }
            };

        // Notifications (no id) get no response per JSON-RPC spec
        if response.id.is_none() && response.result.is_none() && response.error.is_none() {
            continue;
        }

        match serde_json::to_string(&response) {
            Ok(resp_json) => {
                debug!("MCP send: {}", &resp_json[..resp_json.len().min(200)]);
                if stdout.write_all(resp_json.as_bytes()).await.is_err() {
                    break;
                }
                if stdout.write_all(b"\n").await.is_err() {
                    break;
                }
                if stdout.flush().await.is_err() {
                    break;
                }
            }
            Err(e) => {
                error!("Failed to serialize MCP response: {}", e);
            }
        }
    }

    debug!("MCP stdio transport ended (stdin closed)");
}
