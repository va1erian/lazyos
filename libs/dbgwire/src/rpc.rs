//! The JSON-RPC 2.0 subset `dbgd` speaks: one JSON object per line, requests
//! with an `id`, answers carrying the same `id`, and notifications (no `id`)
//! for the streamed log. Batches are not accepted.

use alloc::string::String;

use crate::json::{self, Value};

/// Longest request line (bytes, the newline excluded).
pub const MAX_LINE: usize = 16 * 1024;

/// JSON-RPC and `dbgd` error codes.
pub mod code {
    pub const PARSE: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL: i32 = -32603;
    /// A method that needs authentication, before `auth`.
    pub const UNAUTHENTICATED: i32 = -32001;
    /// Refused by policy (a path outside the allowlist, a control method
    /// with control disabled).
    pub const DENIED: i32 = -32002;
    /// The data source is not there (no `devd`, no such log).
    pub const UNAVAILABLE: i32 = -32003;
    /// Too many failed attempts: wait and retry.
    pub const LOCKED_OUT: i32 = -32004;
}

/// A request `id`: a number, a string or `null`, echoed back as given.
#[derive(Clone, Debug, PartialEq)]
pub enum Id {
    Null,
    Int(i64),
    Str(String),
}

impl Id {
    fn json(&self) -> String {
        match self {
            Id::Null => String::from("null"),
            Id::Int(n) => alloc::format!("{n}"),
            Id::Str(text) => json::quoted(text),
        }
    }
}

/// A well-formed request.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// `None` for a notification, which gets no answer.
    pub id: Option<Id>,
    pub method: String,
    /// The `params` object, or an empty object when absent.
    pub params: Value,
}

/// A request that could not be used, with what to answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub id: Id,
    pub code: i32,
    pub message: &'static str,
}

fn invalid(id: Id, message: &'static str) -> Failure {
    Failure {
        id,
        code: code::INVALID_REQUEST,
        message,
    }
}

/// Parse one request line.
pub fn parse_request(line: &str) -> Result<Request, Failure> {
    let value = json::parse(line).map_err(|_| Failure {
        id: Id::Null,
        code: code::PARSE,
        message: "not valid JSON (or too deep or too long)",
    })?;
    let Value::Object(_) = value else {
        return Err(invalid(Id::Null, "a request is a JSON object"));
    };
    let id = match value.get("id") {
        None => None,
        Some(Value::Null) => Some(Id::Null),
        Some(Value::Int(n)) => Some(Id::Int(*n)),
        Some(Value::Str(text)) if text.len() <= 64 => Some(Id::Str(text.clone())),
        Some(_) => return Err(invalid(Id::Null, "id is a number, a string or null")),
    };
    let answer_id = id.clone().unwrap_or(Id::Null);
    match value.get("jsonrpc") {
        Some(Value::Str(version)) if version == "2.0" => {}
        _ => return Err(invalid(answer_id, "jsonrpc must be \"2.0\"")),
    }
    let method = match value.get("method") {
        Some(Value::Str(name)) if !name.is_empty() && name.len() <= 64 => name.clone(),
        _ => return Err(invalid(answer_id, "method is a short string")),
    };
    let params = match value.get("params") {
        None | Some(Value::Null) => Value::Object(alloc::vec::Vec::new()),
        Some(object @ Value::Object(_)) => object.clone(),
        Some(_) => {
            return Err(Failure {
                id: answer_id,
                code: code::INVALID_PARAMS,
                message: "params is an object",
            })
        }
    };
    Ok(Request { id, method, params })
}

/// A success answer line (no trailing newline); `result` is JSON.
pub fn result_line(id: &Id, result: &str) -> String {
    alloc::format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{result}}}",
        id.json()
    )
}

/// An error answer line (no trailing newline).
pub fn error_line(id: &Id, code: i32, message: &str) -> String {
    alloc::format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":{code},\"message\":{}}}}}",
        id.json(),
        json::quoted(message)
    )
}

/// A notification line (no trailing newline); `params` is JSON.
pub fn notification_line(method: &str, params: &str) -> String {
    alloc::format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":{},\"params\":{params}}}",
        json::quoted(method)
    )
}
