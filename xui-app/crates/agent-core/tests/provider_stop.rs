//! Stop and stalls against a scripted TLS server (docs/ai-agent-plan.md
//! §5.2): Stop ends a stream within about one poll in every state it can be
//! in: stalled, busy (bytes arriving all the time), in the middle of a record
//! that trickles in, before the first byte, and while an error body stalls.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use common::*;
use lazyagent_core::provider::Discard;
use lazyagent_core::{ChatError, Provider};

/// Sets `stop` after `ms`; the handle gives the moment it was set.
fn stop_after(stop: &Arc<AtomicBool>, ms: u64) -> JoinHandle<Instant> {
    let flag = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(ms));
        flag.store(true, Ordering::Relaxed);
        Instant::now()
    })
}

/// How long after the stop was set the call returned.
fn lag(stopper: JoinHandle<Instant>) -> Duration {
    let ended = Instant::now();
    ended.saturating_duration_since(stopper.join().unwrap())
}

/// A poll period of 250 ms plus room for a loaded machine.
const WITHIN: Duration = Duration::from_millis(700);

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
    let stopper = stop_after(&stop, 500);
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    let lag = lag(stopper);
    let ChatError::Stopped { partial } = err else {
        panic!("{err:?}")
    };
    assert_eq!(partial.content, "so far");
    assert!(lag < WITHIN, "Stop took {lag:?}");
}

#[test]
fn stop_before_the_first_byte_also_works() {
    let pki = Pki::new();
    let server = serve(&pki, |_, _| vec![Step::Sleep(Duration::from_secs(30))]);
    let client = server.client(&pki);
    let stop = no_stop();
    let stopper = stop_after(&stop, 300);
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    assert!(matches!(err, ChatError::Stopped { .. }), "{err:?}");
    assert!(lag(stopper) < WITHIN);
}

#[test]
fn stop_works_while_the_stream_is_busy() {
    // An event every 20 ms for 8 s: no read ever times out, so Stop has to be
    // noticed on the data path (it used to run to the end).
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        let mut steps = vec![sse_head()];
        for n in 0..400 {
            steps.push(event(&text_delta(&format!("w{n} "))));
            steps.push(Step::Sleep(Duration::from_millis(20)));
        }
        steps.push(end());
        steps
    });
    let client = server.client(&pki);
    let stop = no_stop();
    let started = Instant::now();
    let stopper = stop_after(&stop, 1000);
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    let lag = lag(stopper);
    let ChatError::Stopped { partial } = err else {
        panic!("{err:?}")
    };
    assert!(partial.content.starts_with("w0 w1 "), "{}", partial.content);
    assert!(lag < WITHIN, "Stop took {lag:?} on a busy stream");
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn stop_works_while_one_record_trickles_in() {
    // One record sent a byte at a time, 50 ms apart (about 4 s): every socket
    // read succeeds, so neither the poll timeout nor the idle limit ever fires.
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            sse_head(),
            Step::Trickle(
                chunk(&format!("data: {}\n\n", text_delta("slowly"))),
                Duration::from_millis(50),
            ),
            end(),
        ]
    });
    let client = server.client(&pki);
    let stop = no_stop();
    let started = Instant::now();
    let stopper = stop_after(&stop, 1000);
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    let lag = lag(stopper);
    assert!(matches!(err, ChatError::Stopped { .. }), "{err:?}");
    assert!(lag < WITHIN, "Stop took {lag:?} on a trickling record");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn stop_during_a_stalled_error_body_does_not_hang() {
    // `read_to_end` retries on `ErrorKind::Interrupted`; Stop must not look
    // like that, or this never returned.
    let pki = Pki::new();
    let server = serve(&pki, |_, _| {
        vec![
            Step::Send(b"HTTP/1.1 500 X\r\nContent-Length: 1000\r\n\r\nabcd".to_vec()),
            Step::Sleep(Duration::from_secs(30)),
        ]
    });
    let client = server.client(&pki);
    let stop = no_stop();
    let stopper = stop_after(&stop, 500);
    let err = client.stream(&request(), &stop, &mut Discard).unwrap_err();
    assert!(matches!(err, ChatError::Stopped { .. }), "{err:?}");
    assert!(lag(stopper) < WITHIN);
}

#[test]
fn a_reply_that_never_ends_is_cut_at_the_size_cap_and_marked() {
    let pki = Pki::new();
    let piece = "y".repeat(1024);
    let server = serve(&pki, move |_, _| {
        let mut steps = vec![sse_head()];
        for _ in 0..1200 {
            steps.push(event(&text_delta(&piece)));
        }
        steps.extend([event(&finish("stop")), end()]);
        steps
    });
    let client = server.client(&pki);
    let done = client.stream(&request(), &no_stop(), &mut Discard).unwrap();
    assert!(done.truncated);
    assert!(done.content.len() <= 1024 * 1024 && done.content.len() > 1000 * 1024);
}
