//! The Fireworks client against a scripted TLS server (docs/ai-agent-plan.md
//! P1 "Done when"): split events, a dropped stream, a stalled one, Stop
//! during a stall, 429 with `Retry-After`, a 422 body, and refusals of
//! another host, `http:` and a redirect.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use lazyagent_core::message::{Message, ToolSpec};
use lazyagent_core::provider::Discard;
use lazyagent_core::{
    ChatError, ChatRequest, Completion, Effort, Endpoint, Fireworks, Params, Provider, Secret, Sink,
};

fn request() -> ChatRequest {
    ChatRequest {
        model: "accounts/fireworks/models/deepseek-v4p1-flash".into(),
        messages: vec![Message::system("be brief"), Message::user("hi")],
        tools: Vec::new(),
        params: Params {
            reasoning_effort: Some(Effort::Low),
            ..Params::default()
        },
    }
}

fn no_stop() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[derive(Default)]
struct Recorder {
    text: String,
    reasoning: String,
    deltas: usize,
    retries: Vec<(u32, Duration, String)>,
}

impl Sink for Recorder {
    fn text(&mut self, t: &str) {
        self.text.push_str(t);
    }
    fn reasoning(&mut self, t: &str) {
        self.reasoning.push_str(t);
    }
    fn tool_call_delta(&mut self, _: usize, _: Option<&str>, _: Option<&str>, _: &str) {
        self.deltas += 1;
    }
    fn retrying(&mut self, attempt: u32, delay: Duration, why: &str) {
        self.retries.push((attempt, delay, why.to_string()));
    }
}

fn fast(client: &mut Fireworks) {
    client.retry.base = Duration::from_millis(20);
    client.retry.cap = Duration::from_secs(3);
}

#[test]
fn a_reply_streams_with_reasoning_and_usage() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            sse_head(),
            event(&reasoning_delta("The user ")),
            event(&reasoning_delta("greets.")),
            event(&text_delta("Hello")),
            event(&text_delta(" there")),
            event(&finish("stop")),
            event(&usage(36, 192, 183)),
            Step::Send(chunk("data: [DONE]\n\n")),
            end(),
        ]
    });
    let client = server.client(&pki);
    let mut sink = Recorder::default();
    let done = client.stream(&request(), &no_stop(), &mut sink).unwrap();
    assert_eq!(done.content, "Hello there");
    assert_eq!(done.reasoning, "The user greets.");
    assert_eq!(sink.text, "Hello there");
    assert_eq!(sink.reasoning, "The user greets.");
    assert_eq!(done.finish_reason.as_deref(), Some("stop"));
    let usage = done.usage.unwrap();
    assert_eq!(
        (usage.prompt, usage.completion, usage.reasoning),
        (36, 192, 183)
    );
    assert!(done.first_chunk_ms.is_some());
    assert_eq!(done.server_ttft_ms, Some(250));

    let seen = server.seen.lock().unwrap()[0].clone();
    assert_eq!(
        seen.request_line,
        "POST /inference/v1/chat/completions HTTP/1.1"
    );
    assert_eq!(
        seen.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(seen.header("accept"), Some("text/event-stream"));
    let body = seen.json();
    assert_eq!(
        body["model"],
        "accounts/fireworks/models/deepseek-v4p1-flash"
    );
    assert_eq!(body["stream"], true);
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(body["messages"][1]["content"], "hi");
    assert!(
        !String::from_utf8_lossy(&seen.body).contains(KEY),
        "the key is never in a body"
    );
}

#[test]
fn streamed_tool_calls_are_joined_by_index() {
    let pki = Pki::new();
    let call = |index: usize, id: Option<&str>, name: Option<&str>, args: &str| {
        let mut call = serde_json::json!({"index": index, "function": {"arguments": args}});
        if let Some(id) = id {
            call["id"] = id.into();
            call["type"] = "function".into();
        }
        if let Some(name) = name {
            call["function"]["name"] = name.into();
        }
        serde_json::json!({"choices":[{"delta":{"tool_calls":[call]}}]}).to_string()
    };
    let server = serve(&pki, move |_, _| {
        vec![
            sse_head(),
            event(&call(0, Some("call_a"), Some("get_service"), "")),
            event(&call(0, None, None, "{\"name\":\"net")),
            event(&call(1, Some("call_b"), Some("kernel_log"), "{\"count\":")),
            event(&call(0, None, None, "d\"}")),
            event(&call(1, None, None, "20}")),
            event(&finish("tool_calls")),
            event(&usage(365, 105, 14)),
            Step::Send(chunk("data: [DONE]\n\n")),
            end(),
        ]
    });
    let mut req = request();
    req.tools = vec![ToolSpec {
        name: "get_service".into(),
        description: "d".into(),
        parameters: serde_json::json!({"type":"object","properties":{"name":{"type":"string"}}}),
    }];
    let mut sink = Recorder::default();
    let done = server
        .client(&pki)
        .stream(&req, &no_stop(), &mut sink)
        .unwrap();
    assert_eq!(done.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(done.tool_calls.len(), 2);
    assert_eq!(done.tool_calls[0].id, "call_a");
    assert_eq!(
        done.tool_calls[0].parsed_arguments().unwrap()["name"],
        "netd"
    );
    assert_eq!(done.tool_calls[1].parsed_arguments().unwrap()["count"], 20);
    assert_eq!(sink.deltas, 5);
    let sent = server.seen.lock().unwrap()[0].json();
    assert_eq!(sent["tools"][0]["function"]["name"], "get_service");
}

#[test]
fn events_split_inside_a_tls_record_arrive_whole() {
    let pki = Pki::new();
    let long = "word ".repeat(300);
    let server = serve(&pki, {
        let long = long.clone();
        move |_, _| {
            vec![
                sse_head(),
                Step::SplitSend(
                    chunk(&format!("data: {}\n\n", text_delta(&long))),
                    Duration::from_millis(400),
                ),
                event(&finish("stop")),
                Step::Send(chunk("data: [DONE]\n\n")),
                end(),
            ]
        }
    });
    let mut client = server.client(&pki);
    client.poll = Duration::from_millis(50);
    let done = client.stream(&request(), &no_stop(), &mut Discard).unwrap();
    assert_eq!(done.content, long);
}

#[test]
fn a_stream_that_just_ends_keeps_what_arrived_as_interrupted() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![sse_head(), event(&text_delta("Half an ans"))]
    });
    let err = server
        .client(&pki)
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    let ChatError::Interrupted { partial, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(partial.content, "Half an ans");
    assert_eq!(err.status(), "interrupted");
}

#[test]
fn a_clean_end_with_a_finish_reason_but_no_done_is_accepted() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            sse_head(),
            event(&text_delta("ok")),
            event(&finish("stop")),
            end(),
        ]
    });
    let done = server
        .client(&pki)
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap();
    assert_eq!(done.content, "ok");
}

#[test]
fn a_mid_stream_error_event_interrupts_with_the_partial() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            sse_head(),
            event(&text_delta("abc")),
            event(r#"{"error":{"message":"model overloaded"}}"#),
            end(),
        ]
    });
    let err = server
        .client(&pki)
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    let ChatError::Interrupted { partial, why } = err else {
        panic!()
    };
    assert_eq!(partial.content, "abc");
    assert!(why.contains("model overloaded"), "{why}");
}

#[test]
fn a_stalled_stream_is_interrupted_after_the_idle_limit() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            sse_head(),
            event(&text_delta("start")),
            Step::Sleep(Duration::from_secs(30)),
        ]
    });
    let mut client = server.client(&pki);
    client.idle = Duration::from_millis(700);
    client.poll = Duration::from_millis(50);
    let started = Instant::now();
    let err = client
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    let waited = started.elapsed();
    let ChatError::Interrupted { partial, why } = err else {
        panic!()
    };
    assert_eq!(partial.content, "start");
    assert!(why.contains("no data"), "{why}");
    assert!(
        waited >= Duration::from_millis(700) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
}

#[test]
fn stop_during_a_stall_takes_effect_within_a_poll() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            sse_head(),
            event(&text_delta("so far")),
            Step::Sleep(Duration::from_secs(30)),
        ]
    });
    let client = server.client(&pki);
    assert_eq!(
        client.poll,
        Duration::from_millis(250),
        "the plan's poll period"
    );
    let stop = no_stop();
    let flag = stop.clone();
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        flag.store(true, Ordering::Relaxed);
        Instant::now()
    });
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    let ended = Instant::now();
    let stopped_at = stopper.join().unwrap();
    let ChatError::Stopped { partial } = err else {
        panic!("{err:?}")
    };
    assert_eq!(partial.content, "so far");
    let lag = ended.duration_since(stopped_at);
    assert!(lag < Duration::from_millis(400), "Stop took {lag:?}");
}

#[test]
fn stop_before_the_first_byte_also_works() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| vec![Step::Sleep(Duration::from_secs(30))]);
    let client = server.client(&pki);
    let stop = no_stop();
    let flag = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    assert!(matches!(err, ChatError::Stopped { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_429_waits_for_retry_after_and_then_succeeds() {
    let pki = Pki::new();
    let server = serve(&pki, |n, _| {
        if n == 0 {
            status(
                429,
                &[("Retry-After", "1")],
                r#"{"error":{"message":"slow down"}}"#,
            )
        } else {
            answer_of("through")
        }
    });
    let mut client = server.client(&pki);
    fast(&mut client);
    let mut sink = Recorder::default();
    let started = Instant::now();
    let done = client.stream(&request(), &no_stop(), &mut sink).unwrap();
    assert_eq!(done.content, "through");
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "waited {:?}",
        started.elapsed()
    );
    assert_eq!(sink.retries.len(), 1);
    assert_eq!(sink.retries[0].1, Duration::from_secs(1));
    assert!(sink.retries[0].2.contains("slow down"));
    assert_eq!(server.requests(), 2);
}

#[test]
fn rate_limits_and_server_errors_are_retried_twice_and_then_reported() {
    let pki = Pki::new();
    let limited = serve(&pki, |_, _| {
        status(429, &[], r#"{"error":{"message":"quota"}}"#)
    });
    let mut client = limited.client(&pki);
    fast(&mut client);
    let err = client
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    assert!(
        matches!(&err, ChatError::RateLimited { detail, .. } if detail == "quota"),
        "{err:?}"
    );
    assert_eq!(limited.requests(), 3, "one try and two retries");

    let broken = serve(&pki, |n, _| {
        if n < 2 {
            status(503, &[], r#"{"error":{"message":"busy"}}"#)
        } else {
            answer_of("third time")
        }
    });
    let mut client = broken.client(&pki);
    fast(&mut client);
    assert_eq!(
        client
            .stream(&request(), &no_stop(), &mut Discard)
            .unwrap()
            .content,
        "third time"
    );
    assert_eq!(broken.requests(), 3);

    let dead = serve(&pki, |_, _| status(500, &[], "plain failure"));
    let mut client = dead.client(&pki);
    fast(&mut client);
    let err = client
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    assert!(
        matches!(&err, ChatError::Server { status: 500, detail } if detail == "plain failure"),
        "{err:?}"
    );
}

#[test]
fn stop_during_a_retry_wait_ends_it() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| status(429, &[("Retry-After", "20")], "{}"));
    let mut client = server.client(&pki);
    client.retry.cap = Duration::from_secs(30);
    let stop = no_stop();
    let flag = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    assert!(matches!(err, ChatError::Stopped { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_422_shows_the_servers_detail_and_is_not_retried() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        status(
            422,
            &[],
            r#"{"detail":[{"loc":["body","temperature"],"msg":"Input should be a valid number","type":"x"}]}"#,
        )
    });
    let err = server
        .client(&pki)
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    let ChatError::Rejected { status, detail } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*status, 422);
    assert_eq!(detail, "body.temperature: Input should be a valid number");
    assert_eq!(server.requests(), 1);
    assert!(err.to_string().contains("(422)"));
}

#[test]
fn a_401_asks_for_the_key_again_and_a_400_is_a_rejection() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        status(
            401,
            &[],
            r#"{"error":{"message":"The API key you provided is invalid.","code":"UNAUTHORIZED"}}"#,
        )
    });
    let err = server
        .client(&pki)
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    assert!(
        matches!(&err, ChatError::Unauthorized(d) if d.contains("invalid")),
        "{err:?}"
    );
    assert_eq!(err.status(), "401");
    assert!(!err.to_string().contains(KEY));
    assert_eq!(server.requests(), 1);

    let server = serve(&pki, |_, _| {
        status(400, &[], r#"{"error":{"message":"bad field"}}"#)
    });
    let err = server
        .client(&pki)
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    assert!(matches!(err, ChatError::Rejected { status: 400, .. }));
}

#[test]
fn a_redirect_is_not_followed() {
    let pki = Pki::new();
    let target = serve(&pki, |_, _| answer_of("should never be fetched"));
    let location = format!(
        "https://localhost:{}/inference/v1/chat/completions",
        target.port
    );
    let origin = serve(&pki, move |_, _| {
        status(307, &[("Location", location.as_str())], "")
    });
    let err = origin
        .client(&pki)
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    assert!(
        matches!(err, ChatError::Rejected { status: 307, .. }),
        "{err:?}"
    );
    assert_eq!(origin.requests(), 1);
    assert_eq!(target.requests(), 0, "the key must never follow a redirect");
}

#[test]
fn another_host_and_plain_http_are_refused_before_any_connection() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| answer_of("never"));
    let port = server.port;
    for base in [
        format!("http://localhost:{port}/inference/v1"),
        format!("https://127.0.0.1:{port}/inference/v1"),
        "https://api.fireworks.ai.evil.test/inference/v1".to_string(),
        format!("https://user:pw@localhost:{port}/inference/v1"),
    ] {
        let client = Fireworks::new(
            pki.roots(),
            Secret::new(KEY),
            Endpoint::custom(&base, &format!("localhost:{port}")),
        );
        let err: ChatError = client
            .stream(&request(), &no_stop(), &mut Discard)
            .unwrap_err();
        assert!(matches!(err, ChatError::Refused(_)), "{base}: {err:?}");
    }
    assert_eq!(server.requests(), 0);
}

#[test]
fn the_production_endpoint_is_https_to_fireworks_only() {
    // Not connected: the certificate store is empty, so this fails as a
    // transport error after the policy accepted the address.
    let client = Fireworks::new(
        lazyagent_core::Roots::Pem(Arc::default()),
        Secret::new(KEY),
        Endpoint::fireworks(),
    );
    let err = client
        .stream(&request(), &no_stop(), &mut Discard)
        .unwrap_err();
    assert!(!matches!(err, ChatError::Refused(_)), "{err:?}");
}

#[test]
fn many_streams_in_a_row_do_not_leak_or_mix() {
    let pki = Pki::new();
    let server = serve(&pki, |n, _| answer_of(&format!("answer {n}")));
    let client = server.client(&pki);
    for n in 0..60 {
        let done: Completion = client.stream(&request(), &no_stop(), &mut Discard).unwrap();
        assert_eq!(done.content, format!("answer {n}"));
    }
}
