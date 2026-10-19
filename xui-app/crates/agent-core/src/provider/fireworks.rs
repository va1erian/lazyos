//! The Fireworks client (docs/ai-agent-plan.md §5): one streaming
//! `chat/completions` request, its status mapping, retries and the stream
//! loop. It talks to one host over HTTPS with no redirects: the policy is
//! built from the [`Endpoint`] and enforced by `lazyhttps`.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lazyhttps::{Http, HttpOptions, Policy, Roots, StreamResponse, Watch};

use super::body::{self, Parsed};
use super::{ChatError, ChatRequest, Completion, Provider, Sink, ToolCallAssembler};
use crate::retry::{parse_retry_after, sleep_unless_stopped, Retry};
use crate::secret::Secret;
use crate::sse::{SseEvent, SseParser};

/// Where requests go.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Everything before `/chat/completions`.
    base_url: String,
    /// The one host (or `host:port`) the policy allows.
    allow: String,
}

impl Endpoint {
    /// The real service. The URL is a constant, not a setting (§3).
    pub fn fireworks() -> Endpoint {
        Endpoint {
            base_url: "https://api.fireworks.ai/inference/v1".into(),
            allow: "api.fireworks.ai".into(),
        }
    }

    /// Another server for tests and harnesses: `host_port` is `host:port`.
    pub fn for_testing(host_port: &str) -> Endpoint {
        Endpoint::custom(&format!("https://{host_port}/inference/v1"), host_port)
    }

    /// A base URL and the one host (`host` or `host:port`) the policy lets
    /// it reach. A mismatch is allowed here so tests can check the policy
    /// refuses it; the app only ever uses [`Endpoint::fireworks`].
    pub fn custom(base_url: &str, allow: &str) -> Endpoint {
        Endpoint {
            base_url: base_url.trim_end_matches('/').to_string(),
            allow: allow.to_ascii_lowercase(),
        }
    }
}

pub struct Fireworks {
    http: Http,
    endpoint: Endpoint,
    key: Secret,
    pub retry: Retry,
    /// Silence on the stream longer than this is an interruption (§5.2).
    pub idle: Duration,
    /// How often a blocked read wakes to look at Stop (§5.2).
    pub poll: Duration,
}

/// The most of an error body that is read.
const MAX_ERROR_BODY: u64 = 64 * 1024;

impl Fireworks {
    /// Like the `Http` it holds, a `Fireworks` belongs to one thread.
    pub fn new(roots: Roots, key: Secret, endpoint: Endpoint) -> Fireworks {
        let options = HttpOptions {
            user_agent: concat!("LazyAgent/", env!("CARGO_PKG_VERSION")).to_string(),
            policy: Policy::https_hosts(&[&endpoint.allow]),
            ..HttpOptions::default()
        };
        Fireworks {
            http: Http::new(roots, options),
            endpoint,
            key,
            retry: Retry::default(),
            idle: Duration::from_secs(60),
            poll: Duration::from_millis(250),
        }
    }

    fn attempt(
        &self,
        request: &ChatRequest,
        stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, ChatError> {
        let started = Instant::now();
        let url = format!("{}/chat/completions", self.endpoint.base_url);
        let bearer = format!("Bearer {}", self.key.expose());
        let headers = [
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
            ("accept", "text/event-stream"),
        ];
        let watch = Watch {
            stop: stop.clone(),
            idle: self.idle,
            poll: self.poll,
            wakes: Arc::default(),
        };
        let mut response = self
            .http
            .post_stream(&url, &headers, &body::request_body(request), watch)
            .map_err(|why| {
                if stop.load(Ordering::Relaxed) {
                    ChatError::Stopped {
                        partial: Box::default(),
                    }
                } else if why.starts_with(lazyhttps::REFUSED) {
                    ChatError::Refused(why)
                } else {
                    ChatError::Transport(why)
                }
            })?;
        if response.status != 200 {
            return Err(status_error(&mut response));
        }
        let mut state = Stream {
            started,
            done: false,
            assembler: ToolCallAssembler::default(),
            completion: Completion {
                server_ttft_ms: response
                    .header("fireworks-server-time-to-first-token")
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .map(|s| (s * 1000.0) as u64),
                ..Completion::default()
            },
        };
        let mut parser = SseParser::new();
        let mut buffer = vec![0u8; 16 * 1024];
        let mut failure: Option<String> = None;
        loop {
            match response.read(&mut buffer) {
                Ok(0) => {
                    let mut each = |event| state.event(event, sink, &mut failure);
                    if let Err(e) = parser.finish(&mut each) {
                        failure.get_or_insert(e.to_string());
                    }
                    break;
                }
                Ok(n) => {
                    let mut each = |event| state.event(event, sink, &mut failure);
                    if let Err(e) = parser.feed(&buffer[..n], &mut each) {
                        failure.get_or_insert(e.to_string());
                    }
                }
                Err(e) => {
                    if stop.load(Ordering::Relaxed) || e.kind() == io::ErrorKind::Interrupted {
                        return Err(ChatError::Stopped {
                            partial: Box::new(state.finish()),
                        });
                    }
                    failure.get_or_insert(read_failure(&e, self.idle));
                    break;
                }
            }
            if failure.is_some() || state.done {
                break;
            }
        }
        let early = !state.done && state.completion.finish_reason.is_none();
        let mut completion = state.finish();
        match failure {
            Some(why) => Err(ChatError::Interrupted {
                partial: Box::new(completion),
                why,
            }),
            None if early => Err(ChatError::Interrupted {
                partial: Box::new(completion),
                why: "the server closed the stream early".into(),
            }),
            None => {
                completion.total_ms = started.elapsed().as_millis() as u64;
                Ok(completion)
            }
        }
    }
}

impl Provider for Fireworks {
    fn stream(
        &self,
        request: &ChatRequest,
        stop: &Arc<AtomicBool>,
        sink: &mut dyn Sink,
    ) -> Result<Completion, ChatError> {
        let mut attempt = 0;
        loop {
            let error = match self.attempt(request, stop, sink) {
                Err(error) => error,
                done => return done,
            };
            let (retry_after, why) = match &error {
                ChatError::RateLimited {
                    retry_after,
                    detail,
                } => (*retry_after, format!("429: {detail}")),
                ChatError::Server { status, detail } => (None, format!("{status}: {detail}")),
                _ => return Err(error),
            };
            if attempt >= self.retry.max {
                return Err(error);
            }
            let delay = self.retry.delay(attempt, retry_after);
            sink.retrying(attempt + 1, delay, &why);
            if !sleep_unless_stopped(delay, stop) {
                return Err(ChatError::Stopped {
                    partial: Box::default(),
                });
            }
            attempt += 1;
        }
    }
}

/// The running state of one stream.
struct Stream {
    started: Instant,
    done: bool,
    assembler: ToolCallAssembler,
    completion: Completion,
}

impl Stream {
    fn event(&mut self, event: SseEvent, sink: &mut dyn Sink, failure: &mut Option<String>) {
        if failure.is_some() || self.done {
            return;
        }
        let data = event.data.trim();
        if data == "[DONE]" {
            self.done = true;
            return;
        }
        if data.is_empty() {
            return;
        }
        let chunk = match body::parse_chunk(data) {
            Ok(Parsed::Chunk(chunk)) => chunk,
            Ok(Parsed::Error(why)) => {
                *failure = Some(format!("the server reported: {why}"));
                return;
            }
            Err(why) => {
                *failure = Some(format!("an unreadable event ({why})"));
                return;
            }
        };
        if self.completion.first_chunk_ms.is_none() {
            self.completion.first_chunk_ms = Some(self.started.elapsed().as_millis() as u64);
        }
        if let Some(text) = chunk.text {
            self.completion.content.push_str(&text);
            sink.text(&text);
        }
        if let Some(text) = chunk.reasoning {
            self.completion.reasoning.push_str(&text);
            sink.reasoning(&text);
        }
        for f in chunk.tool_calls {
            sink.tool_call_delta(f.index, f.id.as_deref(), f.name.as_deref(), &f.arguments);
            self.assembler
                .add(f.index, f.id.as_deref(), f.name.as_deref(), &f.arguments);
        }
        if let Some(reason) = chunk.finish_reason {
            self.completion.finish_reason = Some(reason);
        }
        if let Some(usage) = chunk.usage {
            sink.usage(&usage);
            self.completion.usage = Some(usage);
        }
    }

    fn finish(self) -> Completion {
        let mut completion = self.completion;
        completion.tool_calls = self.assembler.finish();
        completion.total_ms = self.started.elapsed().as_millis() as u64;
        completion
    }
}

fn read_failure(error: &io::Error, idle: Duration) -> String {
    match error.kind() {
        io::ErrorKind::TimedOut => {
            format!("no data from the server for {} s", idle.as_secs().max(1))
        }
        _ => format!("the connection failed: {error}"),
    }
}

/// Maps a non-200 answer. The body is read (capped) for the server's words.
fn status_error(response: &mut StreamResponse) -> ChatError {
    let mut body = Vec::new();
    // A broken body only costs the detail.
    let _ = response.take(MAX_ERROR_BODY).read_to_end(&mut body);
    let detail = body::error_detail(&body);
    match response.status {
        401 | 403 => ChatError::Unauthorized(detail),
        429 => ChatError::RateLimited {
            retry_after: response.header("retry-after").and_then(parse_retry_after),
            detail,
        },
        status @ 500..=599 => ChatError::Server { status, detail },
        status @ 300..=399 => ChatError::Rejected {
            status,
            detail: "the server redirected the request, which is refused".into(),
        },
        status => ChatError::Rejected { status, detail },
    }
}
