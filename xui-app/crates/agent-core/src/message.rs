//! What is said in a conversation, in the shape the provider takes it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// A function call the model asked for. `arguments` is the model's own text:
/// it may not be valid JSON, so it is parsed (and checked against the tool's
/// schema) only when the call is about to run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    /// The arguments as a JSON object; an empty string is `{}`.
    pub fn parsed_arguments(&self) -> Result<serde_json::Value, String> {
        if self.arguments.trim().is_empty() {
            return Ok(serde_json::json!({}));
        }
        match serde_json::from_str::<serde_json::Value>(&self.arguments) {
            Ok(value @ serde_json::Value::Object(_)) => Ok(value),
            Ok(_) => Err("the arguments are not a JSON object".into()),
            Err(e) => Err(format!("the arguments are not valid JSON: {e}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Only on an assistant message.
    pub tool_calls: Vec<ToolCall>,
    /// Only on a tool message: the call it answers.
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Message {
        Message::plain(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Message {
        Message::plain(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Message {
        Message {
            tool_calls,
            ..Message::plain(Role::Assistant, content)
        }
    }

    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Message {
        Message {
            tool_call_id: Some(call_id.into()),
            ..Message::plain(Role::Tool, content)
        }
    }

    fn plain(role: Role, content: impl Into<String>) -> Message {
        Message {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// A function the model may call (function tools only).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// A JSON Schema object.
    pub parameters: serde_json::Value,
}

/// Token counts of one request, from the stream's last chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt: u64,
    pub completion: u64,
    pub reasoning: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(arguments: &str) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: "t".into(),
            arguments: arguments.into(),
        }
    }

    #[test]
    fn arguments_must_be_a_json_object() {
        assert!(call("").parsed_arguments().unwrap().is_object());
        assert_eq!(call("{\"a\":1}").parsed_arguments().unwrap()["a"], 1);
        assert!(call("{\"a\":")
            .parsed_arguments()
            .unwrap_err()
            .contains("valid JSON"));
        assert!(call("[1]")
            .parsed_arguments()
            .unwrap_err()
            .contains("object"));
    }
}
