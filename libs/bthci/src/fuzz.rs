//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/bthci.rs`).
//!
//! [`run`] uses the bytes three ways:
//!
//! * as one event packet and as one command packet and as one ACL packet:
//!   parsing must not panic and a packet that parses must survive
//!   encode-then-parse unchanged;
//! * as a script of frames (`u8` length, then an event packet) fed to the
//!   init state machine, the command queue and the ACL buffer pool, which
//!   must keep their bounds whatever a controller says.

use std::vec::Vec;

use crate::acl;
use crate::command::Command;
use crate::event::Event;
use crate::flow::{AclBuffers, CommandQueue, MAX_CREDITS, MAX_QUEUED_COMMANDS};
use crate::init::{Init, InitConfig, Input, Phase};

pub fn run(data: &[u8]) {
    round_trip_event(data);
    round_trip_command(data);
    if let Ok((h, payload)) = acl::parse(data) {
        let again = acl::encode(h.handle, h.boundary, payload).expect("fits");
        assert_eq!(
            acl::parse(&again).map(|(h, p)| (h, p.to_vec())),
            Ok((h, payload.to_vec()))
        );
    }
    let _ = run_script(data);
}

fn round_trip_event(data: &[u8]) {
    if let Ok(ev) = Event::parse(data) {
        let again = Event::parse(&ev.encode()).expect("an encoded event parses");
        assert_eq!(again, ev);
    }
}

fn round_trip_command(data: &[u8]) {
    if let Ok(cmd) = Command::parse(data) {
        let again = Command::parse(&cmd.encode()).expect("an encoded command parses");
        assert_eq!(again, cmd);
    }
}

/// Split `data` into `u8`-length-prefixed frames, stopping at a short one.
fn frames(mut data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while let Some((&len, rest)) = data.split_first() {
        let len = usize::from(len);
        if rest.len() < len || out.len() >= 64 {
            break;
        }
        out.push(&rest[..len]);
        data = &rest[len..];
    }
    out
}

/// Run the controller-event script; returns where the init machine ended.
pub fn run_script(data: &[u8]) -> Phase {
    let cfg = InitConfig {
        le_accept_list: std::vec![(0, crate::BdAddr([1, 2, 3, 4, 5, 6]))],
        ..InitConfig::default()
    };
    let mut init = Init::new(&cfg);
    let mut queue = CommandQueue::new();
    let mut bufs = AclBuffers::new(27, 8);
    let mut now = 0u64;
    let step = init.step(now, Input::Start);
    if let Some(c) = step.send {
        queue.push(&c).expect("room");
    }
    for frame in frames(data) {
        now += 10;
        let Ok(ev) = Event::parse(frame) else {
            continue;
        };
        queue.on_event(&ev);
        bufs.on_completed(&ev);
        let _ = bufs.acquire(u16::from(frame.first().copied().unwrap_or(0)));
        if let Some(c) = init.step(now, Input::Event(&ev)).send {
            let _ = queue.push(&c);
        }
        if let Event::DisconnectionComplete { handle, .. } = ev {
            bufs.link_closed(handle);
        }
        let _ = queue.pop_ready(now);
        assert!(queue.credits() <= MAX_CREDITS && queue.queued() <= MAX_QUEUED_COMMANDS);
        assert!(bufs.free() <= 8);
    }
    let _ = init.step(now + 10_000, Input::Tick);
    let phase = init.phase();
    assert!(matches!(
        phase,
        Phase::FastPath | Phase::SlowPath | Phase::Complete | Phase::Failed(_)
    ));
    phase
}
