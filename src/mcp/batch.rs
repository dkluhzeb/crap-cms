//! JSON-RPC 2.0 batch handling, shared by the stdio and HTTP transports.
//!
//! A client may send an array of request objects instead of a single one
//! (JSON-RPC 2.0 §6). Each member is dispatched independently and the
//! responses come back as an array, in the order the members were sent.
//! Members that are notifications contribute no response, so a batch made
//! up entirely of notifications produces no reply at all.

use serde_json::{Value, from_value, to_value};
use tracing::error;

use crate::mcp::{
    McpServer,
    protocol::{INTERNAL_ERROR, INVALID_REQUEST, JsonRpcRequest, JsonRpcResponse},
};

/// One incoming JSON-RPC payload, after the outer JSON parse.
pub(crate) enum Payload {
    /// A lone request object.
    Single(Box<JsonRpcRequest>),
    /// An array of request objects, still unparsed so that one malformed
    /// member fails on its own rather than poisoning the whole batch.
    Batch(Vec<Value>),
}

/// Classify a parsed JSON body as a single request or a batch.
///
/// # Errors
/// When the body is not an array and does not deserialize into a request.
pub(crate) fn classify(body: Value) -> Result<Payload, serde_json::Error> {
    if let Value::Array(members) = body {
        return Ok(Payload::Batch(members));
    }

    from_value(body).map(|req| Payload::Single(Box::new(req)))
}

/// Serialize a response for inclusion in the outgoing array.
///
/// A failure here is unreachable — every field is already a `Value` — but
/// dropping the member silently would leave the client waiting forever on
/// that id, so it degrades to an internal error carrying the same id.
fn encode(response: JsonRpcResponse) -> Option<Value> {
    let id = response.id.clone();

    match to_value(response) {
        Ok(value) => Some(value),
        Err(e) => {
            error!("MCP batch response could not be serialized: {e}");

            to_value(JsonRpcResponse::error(id, INTERNAL_ERROR, "Internal error")).ok()
        }
    }
}

/// Dispatch one batch member. `None` when nothing is sent back for it —
/// a notification, which the spec says must never be answered.
fn handle_member(server: &McpServer, member: Value) -> Option<Value> {
    let Ok(request) = from_value::<JsonRpcRequest>(member) else {
        return encode(JsonRpcResponse::error(
            None,
            INVALID_REQUEST,
            "Invalid request",
        ));
    };

    // The handshake establishes the session the rest of the batch would run
    // under, and the HTTP transport returns its session id in a header there
    // is only one of per response. The MCP spec keeps `initialize` out of a
    // batch for exactly that reason; accepting it here would hand back a
    // successful-looking handshake that opened no session.
    if request.method == "initialize" {
        return encode(JsonRpcResponse::error(
            request.id,
            INVALID_REQUEST,
            "initialize must be sent on its own, not inside a batch",
        ));
    }

    let is_notification = request.id.is_none();
    let response = server.handle_message(request);

    if is_notification {
        return None;
    }

    encode(response)
}

/// Dispatch every member of a batch.
///
/// `None` means no reply is sent: every member was a notification. An empty
/// array is itself an invalid request and gets a single (non-array) error
/// response, as does a batch over `[mcp] max_batch_members`.
///
/// The cap is checked BEFORE any member runs, and it counts requests rather
/// than bytes: a handful of kilobytes of `delete_many` calls would otherwise
/// multiply into that many whole-collection deletes under one request.
pub(crate) fn handle_batch(server: &McpServer, members: Vec<Value>) -> Option<Value> {
    if members.is_empty() {
        return encode(JsonRpcResponse::error(
            None,
            INVALID_REQUEST,
            "Empty batch: a JSON-RPC batch must carry at least one request",
        ));
    }

    let limit = server.config.mcp.max_batch_members;
    if limit == 0 {
        return encode(JsonRpcResponse::error(
            None,
            INVALID_REQUEST,
            "Batching is disabled ([mcp] max_batch_members = 0); send one request at a time",
        ));
    }

    if members.len() > limit {
        return encode(JsonRpcResponse::error(
            None,
            INVALID_REQUEST,
            format!(
                "Batch too large: {} requests, the limit is {limit} \
                 ([mcp] max_batch_members)",
                members.len()
            ),
        ));
    }

    let responses: Vec<Value> = members
        .into_iter()
        .filter_map(|m| handle_member(server, m))
        .collect();

    if responses.is_empty() {
        return None;
    }

    Some(Value::Array(responses))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Payload, classify, handle_batch};
    use crate::mcp::test_server::make_server;

    fn batch(body: serde_json::Value) -> Vec<serde_json::Value> {
        match classify(body).expect("classify") {
            Payload::Batch(members) => members,
            Payload::Single(_) => panic!("expected a batch"),
        }
    }

    #[test]
    fn a_lone_object_classifies_as_a_single_request() {
        let payload = classify(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }));
        assert!(matches!(payload, Ok(Payload::Single(_))));
    }

    #[test]
    fn every_member_gets_its_own_response_in_order() {
        let (_tmp, server) = make_server();
        let members = batch(json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            { "jsonrpc": "2.0", "id": 2, "method": "nope" },
            { "jsonrpc": "2.0", "id": 3, "method": "ping" },
        ]));

        let out = handle_batch(&server, members).expect("a reply");
        let arr = out.as_array().expect("array reply");

        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], json!(1));
        assert!(arr[0]["result"].is_object());
        assert_eq!(arr[1]["id"], json!(2));
        assert_eq!(arr[1]["error"]["code"], json!(-32601));
        assert_eq!(arr[2]["id"], json!(3));
    }

    /// A notification contributes no response, so the array is shorter than
    /// the batch — and an all-notification batch is answered with nothing.
    #[test]
    fn notifications_are_never_answered() {
        let (_tmp, server) = make_server();

        let mixed = batch(json!([
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": 7, "method": "ping" },
        ]));
        let out = handle_batch(&server, mixed).expect("a reply");
        let arr = out.as_array().expect("array reply");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], json!(7));

        let silent = batch(json!([{ "jsonrpc": "2.0", "method": "notifications/initialized" }]));
        assert!(handle_batch(&server, silent).is_none());
    }

    /// One malformed member fails on its own; its siblings still run.
    #[test]
    fn a_malformed_member_does_not_poison_the_batch() {
        let (_tmp, server) = make_server();
        let members = batch(json!([
            "not an object",
            { "jsonrpc": "2.0", "id": 2, "method": "ping" },
        ]));

        let out = handle_batch(&server, members).expect("a reply");
        let arr = out.as_array().expect("array reply");

        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["error"]["code"], json!(-32600));
        assert_eq!(arr[0]["id"], json!(null));
        assert!(arr[1]["result"].is_object());
    }

    /// The handshake establishes the session the rest of the batch runs
    /// under, so it cannot be a member of one.
    #[test]
    fn initialize_is_refused_inside_a_batch() {
        let (_tmp, server) = make_server();
        let members = batch(json!([
            { "jsonrpc": "2.0", "id": 1, "method": "initialize",
              "params": { "protocolVersion": "2025-03-26", "capabilities": {} } },
            { "jsonrpc": "2.0", "id": 2, "method": "ping" },
        ]));

        let out = handle_batch(&server, members).expect("a reply");
        let arr = out.as_array().expect("array reply");

        assert_eq!(arr[0]["id"], json!(1));
        assert_eq!(arr[0]["error"]["code"], json!(-32600));
        assert!(
            arr[0]["result"].is_null(),
            "no successful-looking handshake"
        );
        assert!(arr[1]["result"].is_object(), "the sibling still runs");
    }

    /// An empty array is an invalid request, answered with a single error
    /// object rather than an empty array.
    #[test]
    fn an_empty_batch_is_an_invalid_request() {
        let (_tmp, server) = make_server();
        let out = handle_batch(&server, vec![]).expect("a reply");

        assert!(!out.is_array());
        assert_eq!(out["error"]["code"], json!(-32600));
        assert_eq!(out["id"], json!(null));
    }

    /// A zero cap turns batching off rather than accepting an unbounded one.
    #[test]
    fn a_zero_cap_disables_batching() {
        let (_tmp, mut server) = make_server();
        server.config.mcp.max_batch_members = 0;

        let members = batch(json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }]));
        let out = handle_batch(&server, members).expect("a reply");

        assert!(!out.is_array());
        assert_eq!(out["error"]["code"], json!(-32600));
        assert!(
            out["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Batching is disabled")
        );
    }

    /// The member cap bounds the work one request can trigger, independently
    /// of the transport's body-size cap.
    #[test]
    fn an_oversized_batch_is_rejected_whole() {
        let (_tmp, server) = make_server();
        let limit = server.config.mcp.max_batch_members;
        let members: Vec<serde_json::Value> = (0..=limit)
            .map(|i| json!({ "jsonrpc": "2.0", "id": i, "method": "ping" }))
            .collect();

        let out = handle_batch(&server, members).expect("a reply");

        assert!(!out.is_array());
        assert_eq!(out["error"]["code"], json!(-32600));
        assert!(
            out["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Batch too large")
        );
    }
}
