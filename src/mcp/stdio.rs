//! stdio transport for the MCP server.
//! Reads JSON-RPC messages from stdin, writes responses to stdout.

use std::sync::Arc;

use serde_json::{from_str, to_string};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tracing::{debug, error};

use crate::mcp::{
    McpServer,
    protocol::{INTERNAL_ERROR, JsonRpcRequest, JsonRpcResponse, PARSE_ERROR},
};

/// Write a JSON-RPC response line to stdout. Returns `false` if the pipe is broken.
async fn write_response(stdout: &mut Stdout, resp: &JsonRpcResponse) -> bool {
    let Ok(resp_json) = to_string(resp) else {
        error!("Failed to serialize MCP response");
        return true;
    };

    debug!("MCP send: {}", &resp_json[..resp_json.len().min(200)]);

    if stdout.write_all(resp_json.as_bytes()).await.is_err()
        || stdout.write_all(b"\n").await.is_err()
        || stdout.flush().await.is_err()
    {
        return false;
    }

    true
}

/// Dispatch a single JSON-RPC request through `spawn_blocking`.
async fn dispatch(server: &Arc<McpServer>, request: JsonRpcRequest) -> JsonRpcResponse {
    let request_id = request.id.clone();
    let server_clone = Arc::clone(server);

    match tokio::task::spawn_blocking(move || server_clone.handle_message(request)).await {
        Ok(resp) => resp,
        Err(_) => {
            error!("MCP spawn_blocking task panicked");
            JsonRpcResponse::error(request_id, INTERNAL_ERROR, "Internal error")
        }
    }
}

/// Returns `true` if the response is a notification acknowledgement (no reply needed).
fn is_empty_notification(resp: &JsonRpcResponse) -> bool {
    resp.id.is_none() && resp.result.is_none() && resp.error.is_none()
}

/// Run the stdio MCP transport. Reads newline-delimited JSON-RPC from stdin,
/// processes each message, and writes responses to stdout.
#[cfg(not(tarpaulin_include))] // requires interactive stdio
pub async fn run_stdio(server: McpServer) {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();
    let server = Arc::new(server);

    debug!("MCP stdio transport started");

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();

        if line.is_empty() {
            continue;
        }

        debug!("MCP recv: {}", &line[..line.len().min(200)]);

        let request: JsonRpcRequest = match from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let resp = JsonRpcResponse::error(None, PARSE_ERROR, format!("Parse error: {e}"));
                write_response(&mut stdout, &resp).await;
                continue;
            }
        };

        let response = dispatch(&server, request).await;

        if is_empty_notification(&response) {
            continue;
        }

        if !write_response(&mut stdout, &response).await {
            break;
        }
    }

    debug!("MCP stdio transport ended (stdin closed)");
}
