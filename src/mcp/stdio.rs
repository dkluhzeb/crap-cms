//! stdio transport for the MCP server.
//! Reads JSON-RPC messages from stdin, writes responses to stdout.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, from_str, to_string, to_value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt, BufReader, Stdout};
use tracing::{debug, error};

use crate::mcp::{
    McpServer,
    batch::{Payload, classify, handle_batch},
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

/// Write a JSON-RPC response line to stdout — a single response object or a
/// batch array. Returns `false` if the pipe is broken.
async fn write_response<T: Serialize>(stdout: &mut Stdout, resp: &T) -> bool {
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

/// Dispatch a whole batch on one blocking hop. `None` when the batch was
/// all notifications and nothing is written back.
async fn dispatch_batch(server: &Arc<McpServer>, members: Vec<Value>) -> Option<Value> {
    let server_clone = Arc::clone(server);

    let Ok(out) = tokio::task::spawn_blocking(move || handle_batch(&server_clone, members)).await
    else {
        error!("MCP spawn_blocking task panicked");

        return to_value(JsonRpcResponse::error(
            None,
            INTERNAL_ERROR,
            "Internal error",
        ))
        .ok();
    };

    out
}

/// What [`read_capped_line`] found on the input stream.
enum CappedLine {
    /// A complete line, newline stripped.
    Line(Vec<u8>),
    /// The line passed the cap. It was drained to its newline and dropped
    /// rather than buffered, so the caller answers an error and reads on.
    TooLong,
    /// The stream ended.
    Eof,
}

/// Read one newline-delimited line, refusing to buffer more than `max` bytes.
///
/// `AsyncBufReadExt::lines` grows its buffer without a ceiling, so a single
/// unterminated line is unbounded memory — the HTTP transport caps the same
/// payload at `[mcp] http_max_body_bytes`. Past the cap the rest of the line
/// is consumed but not kept, so the transport resynchronizes on the next
/// newline instead of misreading the tail as a new message.
async fn read_capped_line<R: AsyncBufRead + Unpin>(reader: &mut R, max: usize) -> CappedLine {
    let mut line: Vec<u8> = Vec::new();
    let mut over_cap = false;

    loop {
        let Ok(available) = reader.fill_buf().await else {
            return CappedLine::Eof;
        };

        if available.is_empty() {
            // EOF: a trailing line without a newline is still a line.
            if over_cap {
                return CappedLine::TooLong;
            }

            return if line.is_empty() {
                CappedLine::Eof
            } else {
                CappedLine::Line(line)
            };
        }

        let newline = available.iter().position(|&b| b == b'\n');
        let (chunk, consumed) = match newline {
            Some(pos) => (&available[..pos], pos + 1),
            None => (available, available.len()),
        };

        if !over_cap {
            if line.len() + chunk.len() > max {
                over_cap = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }

        reader.consume(consumed);

        if newline.is_some() {
            return if over_cap {
                CappedLine::TooLong
            } else {
                CappedLine::Line(line)
            };
        }
    }
}

/// Answer a JSON-RPC parse error on its own line. Returns `false` when the
/// output pipe broke.
async fn answer_parse_error(stdout: &mut Stdout, message: String) -> bool {
    let resp = JsonRpcResponse::error(None, PARSE_ERROR, message);

    write_response(stdout, &resp).await
}

/// Parse one input line into a payload, answering a parse error directly.
/// `None` means the line was already answered and should be skipped.
async fn parse_line(stdout: &mut Stdout, line: &str) -> Option<Payload> {
    let parsed = from_str(line)
        .map_err(|e| e.to_string())
        .and_then(|body| classify(body).map_err(|e| e.to_string()));

    match parsed {
        Ok(payload) => Some(payload),
        Err(e) => {
            answer_parse_error(stdout, format!("Parse error: {e}")).await;
            None
        }
    }
}

/// Handle one payload. Returns `false` when the output pipe broke and the
/// transport should stop.
async fn handle_payload(server: &Arc<McpServer>, stdout: &mut Stdout, payload: Payload) -> bool {
    match payload {
        // An array is a JSON-RPC batch: every member runs, and only the
        // non-notification members answer.
        Payload::Batch(members) => {
            let Some(out) = dispatch_batch(server, members).await else {
                return true;
            };

            write_response(stdout, &out).await
        }

        Payload::Single(request) => {
            // A request with no `id` is a JSON-RPC notification — dispatch it
            // for its side effects but never reply (spec: MUST NOT reply).
            let is_notification = request.id.is_none();
            let response = dispatch(server, *request).await;

            if is_notification {
                return true;
            }

            write_response(stdout, &response).await
        }
    }
}

/// Decode, parse, and dispatch one input line. Returns `false` when the
/// output pipe broke and the transport should stop.
async fn handle_line(server: &Arc<McpServer>, stdout: &mut Stdout, bytes: Vec<u8>) -> bool {
    let Ok(text) = String::from_utf8(bytes) else {
        return answer_parse_error(
            stdout,
            "Parse error: input line is not valid UTF-8".to_string(),
        )
        .await;
    };

    let line = text.trim();

    if line.is_empty() {
        return true;
    }

    debug!("MCP recv: {}", log_preview(line, 200));

    let Some(payload) = parse_line(stdout, line).await else {
        return true;
    };

    handle_payload(server, stdout, payload).await
}

/// Run the stdio MCP transport. Reads newline-delimited JSON-RPC from stdin,
/// processes each message, and writes responses to stdout.
///
/// Lines are capped at `[mcp] http_max_body_bytes` — the same ceiling the HTTP
/// transport puts on a request body — so one unterminated line cannot grow the
/// read buffer without bound.
#[cfg(not(tarpaulin_include))] // requires interactive stdio
pub async fn run_stdio(server: McpServer) {
    let max_line_bytes =
        usize::try_from(server.config.mcp.http_max_body_bytes).unwrap_or(usize::MAX);
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let server = Arc::new(server);

    debug!("MCP stdio transport started");

    loop {
        let bytes = match read_capped_line(&mut reader, max_line_bytes).await {
            CappedLine::Eof => break,
            CappedLine::TooLong => {
                let msg = format!(
                    "Parse error: input line exceeds the {max_line_bytes}-byte limit \
                     (raise [mcp] http_max_body_bytes)"
                );

                if answer_parse_error(&mut stdout, msg).await {
                    continue;
                }

                break;
            }
            CappedLine::Line(bytes) => bytes,
        };

        if !handle_line(&server, &mut stdout, bytes).await {
            break;
        }
    }

    debug!("MCP stdio transport ended (stdin closed)");
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::{CappedLine, log_preview, read_capped_line};

    /// Read every line the input holds, as `Ok(text)` / `Err(())` for one that
    /// passed the cap.
    async fn read_all(input: &str, max: usize) -> Vec<Result<String, ()>> {
        let bytes = input.as_bytes().to_vec();
        let mut reader = BufReader::new(&bytes[..]);
        let mut out = Vec::new();

        loop {
            match read_capped_line(&mut reader, max).await {
                CappedLine::Eof => return out,
                CappedLine::TooLong => out.push(Err(())),
                CappedLine::Line(b) => out.push(Ok(String::from_utf8(b).expect("utf-8"))),
            }
        }
    }

    #[tokio::test]
    async fn reads_newline_delimited_lines_and_the_unterminated_tail() {
        let lines = read_all("{\"a\":1}\n{\"b\":2}\ntail", 1024).await;
        assert_eq!(
            lines,
            vec![
                Ok("{\"a\":1}".to_string()),
                Ok("{\"b\":2}".to_string()),
                Ok("tail".to_string()),
            ]
        );
    }

    /// Regression: the transport read lines with no ceiling while the HTTP
    /// transport capped the same payload, so one unterminated line was
    /// unbounded memory. An over-long line is reported, not buffered — and the
    /// reader resynchronizes on the next newline, so the message after it is
    /// still read correctly.
    #[tokio::test]
    async fn an_over_long_line_is_refused_and_the_next_one_still_parses() {
        let long = "x".repeat(50);
        let input = format!("{long}\n{{\"ok\":1}}\n");

        let lines = read_all(&input, 10).await;
        assert_eq!(lines, vec![Err(()), Ok("{\"ok\":1}".to_string())]);
    }

    /// The cap is on the line, not on the whole stream: many short lines all
    /// come through.
    #[tokio::test]
    async fn the_cap_applies_per_line() {
        let input = "aaa\nbbb\nccc\n";
        let lines = read_all(input, 4).await;
        assert_eq!(
            lines,
            vec![
                Ok("aaa".to_string()),
                Ok("bbb".to_string()),
                Ok("ccc".to_string())
            ]
        );
    }

    /// An empty stream ends immediately; a blank line is still a line (the
    /// caller skips it).
    #[tokio::test]
    async fn empty_input_is_eof_and_a_blank_line_is_empty() {
        assert!(read_all("", 16).await.is_empty());
        assert_eq!(read_all("\n", 16).await, vec![Ok(String::new())]);
    }

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
