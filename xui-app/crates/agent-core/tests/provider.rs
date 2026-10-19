//! The Fireworks client against a scripted TLS server (docs/ai-agent-plan.md
//! P1 "Done when"): replies, tool calls, split events, a dropped stream and a
//! stalled one. Errors and retries are in `provider_errors.rs`, Stop in
//! `provider_stop.rs`.

mod common;

use std::time::{Duration, Instant};

use common::*;
use lazyagent_core::message::ToolSpec;
use lazyagent_core::provider::Discard;
use lazyagent_core::{ChatError, Completion, Provider};

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
fn many_streams_in_a_row_do_not_leak_or_mix() {
    let pki = Pki::new();
    let server = serve(&pki, |n, _| answer_of(&format!("answer {n}")));
    let client = server.client(&pki);
    for n in 0..60 {
        let done: Completion = client.stream(&request(), &no_stop(), &mut Discard).unwrap();
        assert_eq!(done.content, format!("answer {n}"));
    }
}
