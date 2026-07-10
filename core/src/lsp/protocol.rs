//! JSON-RPC message framing for LSP over stdio.
//!
//! LSP messages are sent with a simple header/body protocol:
//!
//! ```text
//! Content-Length: <bytes>\r\n
//! \r\n
//! <json body>
//! ```
//!
//! This module provides the wire-level types and a small streaming
//! decoder so the read loop can pull complete messages out of the
//! server's stdout without knowing the message shape up front.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC message id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(u64),
    String(String),
}

/// An outgoing or incoming JSON-RPC request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Id,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// A JSON-RPC response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Id,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<ResponseError>,
}

/// A JSON-RPC response error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i32,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// A JSON-RPC notification (no id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Any JSON-RPC message we can receive from the server.
#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

/// Serialize a JSON value into the LSP header/body wire format.
pub fn encode_message(value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = header.into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Try to decode one complete message from the head of `buf`.
///
/// Returns the decoded message and removes the consumed bytes from
/// `buf`. Returns `None` if no complete message is available yet.
pub fn decode_one(buf: &mut Vec<u8>) -> Option<Message> {
    // Find the end of the header block.
    let header_end = find_header_end(buf)?;
    let header = std::str::from_utf8(&buf[..header_end]).ok()?;

    let content_length = parse_content_length(header)?;
    let total_len = header_end + content_length;
    if buf.len() < total_len {
        return None;
    }

    let body = &buf[header_end..total_len];
    let value: Value = serde_json::from_slice(body).ok()?;
    buf.drain(..total_len);

    parse_message(value)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 3 < buf.len() {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i + 4);
        }
        i += 1;
    }
    None
}

fn parse_content_length(header: &str) -> Option<usize> {
    for line in header.lines() {
        if let Some(rest) = line.strip_prefix("Content-Length: ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn parse_message(value: Value) -> Option<Message> {
    // A message is a response if it has a result or error field and an id.
    let has_result = value.get("result").is_some();
    let has_error = value.get("error").is_some();
    let has_id = value.get("id").is_some();

    if has_id && (has_result || has_error) {
        serde_json::from_value(value).ok().map(Message::Response)
    } else if has_id {
        serde_json::from_value(value).ok().map(Message::Request)
    } else {
        serde_json::from_value(value)
            .ok()
            .map(Message::Notification)
    }
}

/// Build a JSON-RPC request value.
pub fn request(id: u64, method: impl Into<String>, params: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method.into(),
        "params": params,
    })
}

/// Build a JSON-RPC notification value.
pub fn notification(method: impl Into<String>, params: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method.into(),
        "params": params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_request() {
        let req = request(1, "textDocument/didOpen", serde_json::json!({"text": "hi"}));
        let encoded = encode_message(&req);
        let mut buf = encoded.clone();
        let decoded = decode_one(&mut buf).unwrap();
        match decoded {
            Message::Request(r) => {
                assert_eq!(r.method, "textDocument/didOpen");
                assert_eq!(r.id, Id::Number(1));
            }
            _ => panic!("expected request"),
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn round_trip_notification() {
        let notif = notification("textDocument/didChange", serde_json::json!({}));
        let encoded = encode_message(&notif);
        let mut buf = encoded;
        let decoded = decode_one(&mut buf).unwrap();
        match decoded {
            Message::Notification(n) => assert_eq!(n.method, "textDocument/didChange"),
            _ => panic!("expected notification"),
        }
    }

    #[test]
    fn decode_two_messages_from_one_buffer() {
        let n1 = notification("a", serde_json::json!({}));
        let n2 = notification("b", serde_json::json!({}));
        let mut buf = encode_message(&n1);
        buf.extend_from_slice(&encode_message(&n2));

        let m1 = decode_one(&mut buf).unwrap();
        let m2 = decode_one(&mut buf).unwrap();
        assert!(decode_one(&mut buf).is_none());

        match (m1, m2) {
            (Message::Notification(a), Message::Notification(b)) => {
                assert_eq!(a.method, "a");
                assert_eq!(b.method, "b");
            }
            _ => panic!("expected notifications"),
        }
    }
}
