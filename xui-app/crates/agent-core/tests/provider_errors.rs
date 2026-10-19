//! Errors, retries and refusals of the Fireworks client against a scripted
//! TLS server: 429 with `Retry-After`, 5xx, a 422 body, a 401, and refusals of
//! another host, `http:` and a redirect.

mod common;

use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use lazyagent_core::provider::Discard;
use lazyagent_core::{ChatError, Endpoint, Fireworks, Provider, Secret};

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
        matches!(&err, ChatError::Server { status: 500, detail, .. } if detail == "plain failure"),
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
fn a_503_with_retry_after_waits_for_it() {
    let pki = Pki::new();
    let server = serve(&pki, |n, _| {
        if n == 0 {
            status(
                503,
                &[("Retry-After", "1")],
                r#"{"error":{"message":"overloaded"}}"#,
            )
        } else {
            answer_of("back")
        }
    });
    let mut client = server.client(&pki);
    fast(&mut client);
    let mut sink = Recorder::default();
    let done = client.stream(&request(), &no_stop(), &mut sink).unwrap();
    assert_eq!(done.content, "back");
    assert_eq!(sink.retries.len(), 1);
    assert_eq!(
        sink.retries[0].1,
        Duration::from_secs(1),
        "the server's wait, not the backoff"
    );
}

#[test]
fn the_production_policy_is_https_to_fireworks_only() {
    // No network: the policy for the constant endpoint accepts its own base
    // URL and nothing else.
    let policy = lazyhttps::Policy::https_hosts(&["api.fireworks.ai"]);
    assert!(policy
        .check("https://api.fireworks.ai/inference/v1/chat/completions")
        .is_ok());
    assert!(policy.check("https://api.github.com/").is_err());
    let _ = Endpoint::fireworks();
}
