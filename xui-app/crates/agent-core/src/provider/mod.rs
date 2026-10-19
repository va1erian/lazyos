//! The provider seam (docs/ai-agent-plan.md §5.2): a request goes in, text,
//! reasoning and tool-call fragments come out through a [`Sink`], and the
//! finished answer is returned. A second provider (P10) is a new impl of
//! [`Provider`], never a fork of the loop.

mod assemble;
mod body;
mod fireworks;

use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

pub use assemble::ToolCallAssembler;
pub use body::request_body;
pub use fireworks::{Endpoint, Fireworks};

use crate::message::{Message, ToolCall, ToolSpec, Usage};

/// How hard a reasoning model thinks (`reasoning_effort`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    None,
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::None => "none",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Effort> {
        Some(match s {
            "none" => Effort::None,
            "low" => Effort::Low,
            "medium" => Effort::Medium,
            "high" => Effort::High,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<Effort>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub params: Params,
}

/// What the stream has said so far, and in the end the whole answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completion {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    /// `stop`, `length`, `tool_calls`, ...
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    /// The answer or the reasoning hit the size cap and the rest was dropped.
    pub truncated: bool,
    /// Milliseconds from sending the request to the first streamed chunk.
    pub first_chunk_ms: Option<u64>,
    /// The server's own `fireworks-server-time-to-first-token`, in ms.
    pub server_ttft_ms: Option<u64>,
    pub total_ms: u64,
}

/// Receives the stream as it arrives. Every method has an empty default.
pub trait Sink {
    fn text(&mut self, _text: &str) {}
    fn reasoning(&mut self, _text: &str) {}
    /// One fragment of tool call `index`; `id` and `name` come with the
    /// first one.
    fn tool_call_delta(
        &mut self,
        _index: usize,
        _id: Option<&str>,
        _name: Option<&str>,
        _arguments: &str,
    ) {
    }
    fn usage(&mut self, _usage: &Usage) {}
    /// A retry is about to wait `delay`; `why` is for the user.
    fn retrying(&mut self, _attempt: u32, _delay: Duration, _why: &str) {}
    /// The oldest `turns` turns were replaced by a summary before this request.
    fn compacted(&mut self, _turns: usize) {}
}

/// A sink that ignores everything.
pub struct Discard;

impl Sink for Discard {}

/// Why a request ended without an answer. Nothing here holds the key.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatError {
    /// 401 or 403: the key is missing, wrong or not allowed.
    Unauthorized(String),
    /// 429. `retry_after` is the server's `Retry-After`.
    RateLimited {
        retry_after: Option<Duration>,
        detail: String,
    },
    /// Any other 4xx; `detail` is the server's own words (a 422's `detail`).
    Rejected { status: u16, detail: String },
    /// 5xx.
    Server {
        status: u16,
        detail: String,
        /// The server's `Retry-After`.
        retry_after: Option<Duration>,
    },
    /// The connection, the TLS handshake or the name lookup failed.
    Transport(String),
    /// The policy refused the address (another host, `http:`).
    Refused(String),
    /// The server sent something that is not the protocol.
    Protocol(String),
    /// The user stopped the stream; `partial` is what had arrived.
    Stopped { partial: Box<Completion> },
    /// The stream broke or stalled after it began; `partial` is what had
    /// arrived and may be shown, marked interrupted.
    Interrupted {
        partial: Box<Completion>,
        why: String,
    },
}

impl ChatError {
    /// The status to show in a marker (`AGENT:CHAT:FAIL:<status>`).
    pub fn status(&self) -> String {
        match self {
            ChatError::Unauthorized(_) => "401".into(),
            ChatError::RateLimited { .. } => "429".into(),
            ChatError::Rejected { status, .. } | ChatError::Server { status, .. } => {
                status.to_string()
            }
            ChatError::Transport(_) => "transport".into(),
            ChatError::Refused(_) => "refused".into(),
            ChatError::Protocol(_) => "protocol".into(),
            ChatError::Stopped { .. } => "stopped".into(),
            ChatError::Interrupted { .. } => "interrupted".into(),
        }
    }

    /// What had arrived before the stream ended, if it began.
    pub fn partial(&self) -> Option<&Completion> {
        match self {
            ChatError::Stopped { partial } | ChatError::Interrupted { partial, .. } => {
                Some(partial)
            }
            _ => None,
        }
    }
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatError::Unauthorized(d) => write!(f, "the API key was refused (401): {d}"),
            ChatError::RateLimited { detail, .. } => write!(f, "rate limited (429): {detail}"),
            ChatError::Rejected { status, detail } => {
                write!(f, "the server refused the request ({status}): {detail}")
            }
            ChatError::Server { status, detail, .. } => {
                write!(f, "the server failed ({status}): {detail}")
            }
            ChatError::Transport(d) | ChatError::Refused(d) | ChatError::Protocol(d) => {
                f.write_str(d)
            }
            ChatError::Stopped { .. } => f.write_str("stopped"),
            ChatError::Interrupted { why, .. } => write!(f, "the answer was interrupted: {why}"),
        }
    }
}

impl std::error::Error for ChatError {}

pub trait Provider {
    /// Streams the answer to `request`. `stop` ends it from another thread
    /// within a poll period. Retries of 429 and 5xx happen inside.
    fn stream(
        &self,
        request: &ChatRequest,
        stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, ChatError>;
}

/// Reads one data payload as the stream loop would; for the fuzz entry point.
#[cfg(any(test, feature = "fuzz"))]
pub fn fuzz_chunk(data: &str, assembler: &mut ToolCallAssembler) {
    if let Ok(body::Parsed::Chunk(chunk)) = body::parse_chunk(data) {
        for f in chunk.tool_calls {
            assembler.add(f.index, f.id.as_deref(), f.name.as_deref(), &f.arguments);
        }
    }
}
