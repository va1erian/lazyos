//! The OpenAI-compatible wire format Fireworks speaks (docs/ai-agent-plan.md
//! §5.1): the request body, one streamed chunk, and an error body.

use serde_json::{json, Value};

use super::ChatRequest;
use crate::message::{Message, Role, Usage};

/// The JSON body of a streaming chat request. Reasoning from earlier turns is
/// never sent back (§4). Fireworks ends the stream with a usage chunk without being asked (§5.1).
pub fn request_body(request: &ChatRequest) -> Vec<u8> {
    let mut body = json!({
        "model": request.model,
        "stream": true,
        "messages": request.messages.iter().map(wire_message).collect::<Vec<_>>(),
    });
    let object = body.as_object_mut().expect("an object");
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({ "type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
            })
            .collect();
        object.insert("tools".into(), Value::Array(tools));
        object.insert("tool_choice".into(), json!("auto"));
    }
    let params = &request.params;
    if let Some(t) = params.temperature {
        object.insert("temperature".into(), json!(t));
    }
    if let Some(n) = params.max_tokens {
        object.insert("max_tokens".into(), json!(n));
    }
    if let Some(e) = params.reasoning_effort {
        object.insert("reasoning_effort".into(), json!(e.as_str()));
    }
    serde_json::to_vec(&body).expect("a JSON value serializes")
}

fn wire_message(message: &Message) -> Value {
    let mut out = json!({ "role": message.role.as_str() });
    let object = out.as_object_mut().expect("an object");
    match message.role {
        Role::Assistant if !message.tool_calls.is_empty() => {
            // An assistant turn that only calls tools has no text.
            let content = (!message.content.is_empty()).then_some(message.content.as_str());
            object.insert("content".into(), json!(content));
            let calls: Vec<Value> = message
                .tool_calls
                .iter()
                .map(|c| {
                    json!({ "id": c.id, "type": "function",
                            "function": { "name": c.name, "arguments": c.arguments } })
                })
                .collect();
            object.insert("tool_calls".into(), Value::Array(calls));
        }
        Role::Tool => {
            object.insert("content".into(), json!(message.content));
            object.insert("tool_call_id".into(), json!(message.tool_call_id));
        }
        _ => {
            object.insert("content".into(), json!(message.content));
        }
    }
    out
}

/// One streamed `chat.completion.chunk`.
#[derive(Debug, Default, PartialEq)]
pub struct Chunk {
    pub text: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolFragment>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

#[derive(Debug, PartialEq)]
pub struct ToolFragment {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

/// What a data line holds.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Chunk(Chunk),
    /// `{"error": ...}` in the middle of a stream.
    Error(String),
}

/// Parses one data payload (not `[DONE]`). The reasoning field is
/// `reasoning_content` (P0, 2026-10-19); `reasoning` is accepted too, as some
/// OpenAI-compatible servers name it so.
pub fn parse_chunk(data: &str) -> Result<Parsed, String> {
    let value: Value = serde_json::from_str(data).map_err(|e| format!("not JSON: {e}"))?;
    if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
        return Ok(Parsed::Error(error_text(error)));
    }
    let mut chunk = Chunk::default();
    if let Some(choice) = value.pointer("/choices/0") {
        let delta = choice.get("delta");
        let field = |name: &str| {
            delta
                .and_then(|d| d.get(name))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        chunk.text = field("content");
        chunk.reasoning = field("reasoning_content").or_else(|| field("reasoning"));
        chunk.finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(calls) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in calls {
                let function = call.get("function");
                let text = |v: Option<&Value>, key: &str| {
                    v.and_then(|f| f.get(key))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                };
                chunk.tool_calls.push(ToolFragment {
                    index: call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                    id: text(Some(call), "id"),
                    name: text(function, "name"),
                    arguments: text(function, "arguments").unwrap_or_default(),
                });
            }
        }
    }
    if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
        let count = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
        chunk.usage = Some(Usage {
            prompt: count(usage.get("prompt_tokens")),
            completion: count(usage.get("completion_tokens")),
            reasoning: count(usage.pointer("/completion_tokens_details/reasoning_tokens")),
        });
    }
    Ok(Parsed::Chunk(chunk))
}

/// The server's own words from an error body: Fireworks answers
/// `{"error":{"message":...}}`, and a 422 `{"detail":[{"loc":..,"msg":..}]}`.
pub fn error_detail(body: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        let text = String::from_utf8_lossy(body);
        return truncate(text.trim(), 400);
    };
    if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
        return truncate(&error_text(error), 400);
    }
    match value.get("detail") {
        Some(Value::String(s)) => truncate(s, 400),
        Some(Value::Array(items)) => {
            let parts: Vec<String> = items
                .iter()
                .map(|item| {
                    let msg = item.get("msg").and_then(Value::as_str).unwrap_or("invalid");
                    match item.get("loc").and_then(Value::as_array) {
                        Some(loc) if !loc.is_empty() => {
                            let path: Vec<String> = loc
                                .iter()
                                .map(|l| l.as_str().map_or_else(|| l.to_string(), str::to_string))
                                .collect();
                            format!("{}: {msg}", path.join("."))
                        }
                        _ => msg.to_string(),
                    }
                })
                .collect();
            truncate(&parts.join("; "), 400)
        }
        _ => value
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| "no detail".into(), |m| truncate(m, 400)),
    }
}

fn error_text(error: &Value) -> String {
    match error {
        Value::String(s) => s.clone(),
        other => other
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| other.to_string(), str::to_string),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ToolCall, ToolSpec};
    use crate::provider::{Effort, Params};

    fn request() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            messages: vec![
                Message::system("sys"),
                Message::user("hi"),
                Message::assistant(
                    "",
                    vec![ToolCall {
                        id: "call_1".into(),
                        name: "get".into(),
                        arguments: "{\"a\":1}".into(),
                    }],
                ),
                Message::tool("call_1", "result"),
            ],
            tools: vec![ToolSpec {
                name: "get".into(),
                description: "d".into(),
                parameters: json!({"type":"object","properties":{}}),
            }],
            params: Params {
                temperature: Some(0.5),
                max_tokens: Some(100),
                reasoning_effort: Some(Effort::Low),
            },
        }
    }

    #[test]
    fn the_request_body_has_the_openai_shape() {
        let body: Value = serde_json::from_slice(&request_body(&request())).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["function"]["name"], "get");
        assert_eq!(body["messages"][2]["content"], Value::Null);
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1}"
        );
        assert_eq!(body["messages"][3]["tool_call_id"], "call_1");
        let mut bare = request();
        bare.tools.clear();
        bare.params = Params::default();
        let body: Value = serde_json::from_slice(&request_body(&bare)).unwrap();
        for absent in [
            "tools",
            "tool_choice",
            "temperature",
            "max_tokens",
            "reasoning_effort",
        ] {
            assert!(body.get(absent).is_none(), "{absent}");
        }
    }

    #[test]
    fn chunks_carry_text_reasoning_tool_fragments_and_usage() {
        let text = r#"{"choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}],"usage":null}"#;
        let Parsed::Chunk(c) = parse_chunk(text).unwrap() else {
            panic!()
        };
        assert_eq!(c.text.as_deref(), Some("Hi"));
        assert_eq!(c.usage, None);
        let reasoning = r#"{"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#;
        let Parsed::Chunk(c) = parse_chunk(reasoning).unwrap() else {
            panic!()
        };
        assert_eq!(c.reasoning.as_deref(), Some("hmm"));
        let tool = r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c","type":"function","function":{"name":"n","arguments":"{\"x"}}]}}]}"#;
        let Parsed::Chunk(c) = parse_chunk(tool).unwrap() else {
            panic!()
        };
        assert_eq!(c.tool_calls[0].index, 1);
        assert_eq!(c.tool_calls[0].name.as_deref(), Some("n"));
        assert_eq!(c.tool_calls[0].arguments, "{\"x");
        let last = r#"{"choices":[],"usage":{"prompt_tokens":36,"completion_tokens":192,"completion_tokens_details":{"reasoning_tokens":183}}}"#;
        let Parsed::Chunk(c) = parse_chunk(last).unwrap() else {
            panic!()
        };
        assert_eq!(
            c.usage,
            Some(Usage {
                prompt: 36,
                completion: 192,
                reasoning: 183
            })
        );
        let stop = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let Parsed::Chunk(c) = parse_chunk(stop).unwrap() else {
            panic!()
        };
        assert_eq!(c.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn mid_stream_errors_and_junk_are_told_apart() {
        assert_eq!(
            parse_chunk(r#"{"error":{"message":"overloaded"}}"#).unwrap(),
            Parsed::Error("overloaded".into())
        );
        assert!(parse_chunk("not json").is_err());
        let Parsed::Chunk(c) = parse_chunk("{}").unwrap() else {
            panic!()
        };
        assert_eq!(c, Chunk::default());
    }

    #[test]
    fn error_bodies_give_the_servers_words() {
        let fw = br#"{"error":{"object":"error","type":"invalid_request_error","message":"Input should be a valid number, field: 'temperature'"}}"#;
        assert!(error_detail(fw).starts_with("Input should be a valid number"));
        let detail = br#"{"detail":[{"loc":["body","temperature"],"msg":"Input should be a valid number","type":"x"}]}"#;
        assert_eq!(
            error_detail(detail),
            "body.temperature: Input should be a valid number"
        );
        assert_eq!(error_detail(br#"{"detail":"nope"}"#), "nope");
        assert_eq!(error_detail(b"plain text"), "plain text");
        assert!(error_detail(&vec![b'x'; 5000]).len() < 500);
    }
}
