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

/// Truncate a string to at most `max` bytes on a UTF-8 char boundary, for log
/// previews. A plain `&s[..max]` byte-slice panics when byte `max` lands
/// mid-character (any non-ASCII payload), which on the recv side would abort
/// the whole stdio transport.
fn log_preview(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let end = (0..=max)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    &s[..end]
}

/// Write a JSON-RPC response line to stdout. Returns `false` if the pipe is broken.
async fn write_response(stdout: &mut Stdout, resp: &JsonRpcResponse) -> bool {
    let Ok(resp_json) = to_string(resp) else {
        error!("Failed to serialize MCP response");
        return true;
    };

    debug!("MCP send: {}", log_preview(&resp_json, 200));

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

    if let Ok(resp) =
        tokio::task::spawn_blocking(move || server_clone.handle_message(request)).await
    {
        resp
    } else {
        error!("MCP spawn_blocking task panicked");
        JsonRpcResponse::error(request_id, INTERNAL_ERROR, "Internal error")
    }
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

        debug!("MCP recv: {}", log_preview(&line, 200));

        let request: JsonRpcRequest = match from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let resp = JsonRpcResponse::error(None, PARSE_ERROR, format!("Parse error: {e}"));
                write_response(&mut stdout, &resp).await;
                continue;
            }
        };

        // A request with no `id` is a JSON-RPC notification — dispatch it for
        // its side effects but never send a response (spec: MUST NOT reply).
        let is_notification = request.id.is_none();

        let response = dispatch(&server, request).await;

        if is_notification {
            continue;
        }

        if !write_response(&mut stdout, &response).await {
            break;
        }
    }

    debug!("MCP stdio transport ended (stdin closed)");
}

#[cfg(test)]
mod tests {
    use super::log_preview;

    /// Regression: a multi-byte char straddling the byte limit must not panic
    /// (a plain `&s[..200]` did, aborting the transport on any non-ASCII line).
    #[test]
    fn log_preview_truncates_on_char_boundary() {
        // `é` is 2 bytes; place it at bytes 199-200 so byte 200 lands mid-char.
        let s = format!("{}\u{e9}tail", "a".repeat(199));
        let preview = log_preview(&s, 200);
        assert!(preview.len() <= 200);
        assert_eq!(preview.len(), 199); // truncated before the split char
    }

    #[test]
    fn log_preview_short_string_is_unchanged() {
        assert_eq!(log_preview("h\u{e9}llo", 200), "h\u{e9}llo");
    }
}
