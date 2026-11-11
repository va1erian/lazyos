//! The helpers over a collecting [`Sink`]: no window is needed to see what a
//! worker posts, and the proxy-over-sink path is the same call.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::Duration;

use super::*;

const WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq)]
enum Msg {
    Progress(u32),
    Done(Result<u32, Failed>),
}

fn inbox<R: Send + 'static>() -> (Sink<R>, Receiver<R>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    (
        Sink::from_fn(move |value| tx.lock().unwrap().send(value).is_ok()),
        rx,
    )
}

#[test]
fn a_result_arrives_as_a_message() {
    let (sink, rx) = inbox();
    spawn(&sink, "t-result", || 7, Msg::Done);
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Done(Ok(7)));
}

#[test]
fn a_panicking_worker_answers_with_its_name_and_text() {
    let (sink, rx) = inbox();
    spawn(&sink, "t-panic", || -> u32 { panic!("boom") }, Msg::Done);
    match rx.recv_timeout(WAIT).unwrap() {
        Msg::Done(Err(failed)) => {
            assert_eq!(failed.worker, "t-panic");
            assert_eq!(failed.reason, "boom");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn progress_comes_before_the_result_and_cancel_is_seen() {
    let (sink, rx) = inbox();
    let (go_tx, go_rx) = channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let job = spawn_with(
        &sink,
        "t-progress",
        move |report: &Report<u32>| {
            report.report(1);
            go_rx.lock().unwrap().recv().unwrap();
            assert!(report.cancelled());
            2
        },
        Msg::Progress,
        Msg::Done,
    );
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Progress(1));
    job.cancel();
    go_tx.send(()).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Done(Ok(2)));
}

#[test]
fn dropping_the_job_cancels_and_detach_does_not() {
    let (sink, rx) = inbox();
    let (go_tx, go_rx) = channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let job = spawn_with(
        &sink,
        "t-drop",
        move |report: &Report<u32>| {
            go_rx.lock().unwrap().recv().unwrap();
            u32::from(report.cancelled())
        },
        Msg::Progress,
        Msg::Done,
    );
    drop(job);
    go_tx.send(()).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Done(Ok(1)));

    let (go_tx, go_rx) = channel::<()>();
    let go_rx = Mutex::new(go_rx);
    spawn_with(
        &sink,
        "t-detach",
        move |report: &Report<u32>| {
            go_rx.lock().unwrap().recv().unwrap();
            u32::from(report.cancelled())
        },
        Msg::Progress,
        Msg::Done,
    )
    .detach();
    go_tx.send(()).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Done(Ok(0)));
}

#[test]
fn paced_reports_skip_the_burst() {
    let (sink, rx) = inbox();
    spawn_with(
        &sink,
        "t-paced",
        |report: &Report<u32>| {
            let sent = (0..1000)
                .filter(|n| report.paced(Duration::from_secs(60), || *n))
                .count();
            sent as u32
        },
        Msg::Progress,
        Msg::Done,
    )
    .detach();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Progress(0));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Done(Ok(1)));
}

#[test]
fn a_queue_serves_in_order_and_the_latest_skips_a_burst() {
    let (sink, rx) = inbox::<u32>();
    let queue = Queue::start(&sink, "t-queue", |n: u32| n * 2).unwrap();
    for n in 1..=3 {
        assert!(queue.send(n));
    }
    let got: Vec<u32> = (0..3).map(|_| rx.recv_timeout(WAIT).unwrap()).collect();
    assert_eq!(got, [2, 4, 6]);

    let (gate_tx, gate_rx) = channel::<()>();
    let (entered_tx, entered_rx) = channel::<()>();
    let (gate, entered) = (Mutex::new(gate_rx), Mutex::new(entered_tx));
    let latest = Queue::start_latest(&sink, "t-latest", move |n: u32| {
        if n == 0 {
            entered.lock().unwrap().send(()).unwrap();
            gate.lock().unwrap().recv().unwrap();
        }
        n
    })
    .unwrap();
    latest.send(0);
    entered_rx.recv_timeout(WAIT).unwrap();
    latest.send(1);
    latest.send(2);
    latest.send(3);
    gate_tx.send(()).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), 0);
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), 3);
}

#[test]
fn a_queue_ends_when_the_window_is_gone() {
    let (sink, rx) = inbox::<u32>();
    drop(rx);
    let queue = Queue::start(&sink, "t-gone", |n: u32| n).unwrap();
    queue.send(1);
    // The worker posts, finds nobody, and ends: later sends fail.
    let deadline = std::time::Instant::now() + WAIT;
    while queue.send(2) {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_bell_posts_the_same_message_each_ring() {
    let (sink, rx) = inbox::<u32>();
    let bell: Sink<()> = Sink::from_fn(move |()| sink.send(9));
    assert!(bell.ring());
    assert!(bell.ring());
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), 9);
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), 9);
}
