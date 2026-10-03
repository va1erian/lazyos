//! The delivery backlog: nothing is lost while it has room, everything it
//! drops is counted and demands a resync, and order is preserved throughout.

use alloc::vec::Vec;

use crate::outbox::{Outbox, Pushed, Sent, CAPACITY};

/// A client inbox that holds `room` events until `drain` empties it (the
/// kernel's per-endpoint queue is 64 deep).
struct Inbox {
    room: usize,
    held: Vec<u32>,
    seen: Vec<u32>,
}

impl Inbox {
    fn new(room: usize) -> Inbox {
        Inbox {
            room,
            held: Vec::new(),
            seen: Vec::new(),
        }
    }

    fn send(&mut self, item: u32) -> Sent {
        if self.held.len() >= self.room {
            return Sent::Full;
        }
        self.held.push(item);
        Sent::Ok
    }

    /// The client gets scheduled and reads everything queued.
    fn drain(&mut self) {
        self.seen.append(&mut self.held);
    }
}

/// `inputd`'s send path: straight out while the backlog is clear, behind it
/// otherwise. `None` when it went straight out.
fn deliver(outbox: &mut Outbox<u32>, inbox: &mut Inbox, item: u32) -> Option<Pushed> {
    if outbox.is_clear() {
        if inbox.send(item) == Sent::Ok {
            return None;
        }
    } else if !outbox.needs_resync() {
        outbox.flush(|item| inbox.send(*item));
    }
    Some(outbox.push(item))
}

/// The client runs until `inputd` has handed it everything.
fn run_client(outbox: &mut Outbox<u32>, inbox: &mut Inbox) {
    loop {
        inbox.drain();
        if outbox.flush(|item| inbox.send(*item)) == Sent::Ok {
            break;
        }
    }
    inbox.drain();
}

#[test]
fn a_full_inbox_queues_instead_of_dropping() {
    let mut outbox = Outbox::new();
    let mut inbox = Inbox::new(64);
    for item in 0..200 {
        let pushed = deliver(&mut outbox, &mut inbox, item);
        assert!(matches!(pushed, None | Some(Pushed::Queued)), "{item}");
    }
    assert_eq!(inbox.held.len(), 64);
    assert_eq!(outbox.len(), 136);
    assert!(!outbox.needs_resync());
    run_client(&mut outbox, &mut inbox);
    assert!(outbox.is_clear());
    assert_eq!(inbox.seen, (0..200).collect::<Vec<_>>());
    assert_eq!(outbox.peak(), 136);
}

#[test]
fn new_events_wait_behind_the_backlog_even_when_the_inbox_has_room() {
    let mut outbox = Outbox::new();
    let mut inbox = Inbox::new(2);
    for item in 0..5 {
        deliver(&mut outbox, &mut inbox, item);
    }
    inbox.drain();
    // Room again, but 2..5 are still queued: 5 must not jump ahead of them.
    deliver(&mut outbox, &mut inbox, 5);
    run_client(&mut outbox, &mut inbox);
    assert!(outbox.is_clear());
    assert_eq!(inbox.seen, (0..6).collect::<Vec<_>>());
}

#[test]
fn capacity_is_exact_and_overflow_drops_everything_counted() {
    let mut outbox = Outbox::new();
    for item in 0..CAPACITY as u32 {
        assert_eq!(outbox.push(item), Pushed::Queued);
    }
    assert!(!outbox.needs_resync());
    assert_eq!(outbox.push(9999), Pushed::Dropped { lost: CAPACITY + 1 });
    assert!(outbox.is_empty());
    assert!(outbox.needs_resync());
    assert!(!outbox.is_clear(), "a resync is owed before direct sends");
    assert_eq!(outbox.lost(), CAPACITY + 1);
    // Newer events queue behind the owed resync and survive it.
    assert_eq!(outbox.push(1), Pushed::Queued);
    outbox.resynced();
    assert!(!outbox.needs_resync());
    let mut out = Vec::new();
    let flushed = outbox.flush(|item| {
        out.push(*item);
        Sent::Ok
    });
    assert_eq!(flushed, Sent::Ok);
    assert_eq!(out, [1]);
    assert!(outbox.is_clear());
}

#[test]
fn a_gone_client_stops_the_flush_and_keeps_the_item() {
    let mut outbox = Outbox::new();
    outbox.push(1u32);
    outbox.push(2);
    let mut calls = 0;
    let flushed = outbox.flush(|_| {
        calls += 1;
        Sent::Gone
    });
    assert_eq!(flushed, Sent::Gone);
    assert_eq!(calls, 1);
    assert_eq!(outbox.len(), 2);
}

/// Soak: a client descheduled for up to three rounds in a row while a typist
/// keeps going in bursts of up to 250 events. The backlog never exceeds
/// 4 x 250 = 1000 < [`CAPACITY`], so nothing may be lost and order holds.
#[test]
fn soak_bursty_client_loses_nothing_below_capacity() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut outbox = Outbox::new();
    let mut inbox = Inbox::new(64);
    let mut sent = 0u32;
    let mut stalled = 0;
    for _round in 0..20_000 {
        for _ in 0..next() % 251 {
            let pushed = deliver(&mut outbox, &mut inbox, sent);
            assert!(
                matches!(pushed, None | Some(Pushed::Queued)),
                "lost at {sent}"
            );
            sent += 1;
        }
        if stalled < 3 && next() % 2 == 0 {
            stalled += 1;
        } else {
            stalled = 0;
            run_client(&mut outbox, &mut inbox);
        }
        assert!(!outbox.needs_resync());
    }
    run_client(&mut outbox, &mut inbox);
    assert_eq!(inbox.seen.len() as u32, sent);
    assert!(inbox.seen.windows(2).all(|pair| pair[1] == pair[0] + 1));
}

/// Soak: a client that never runs. Its inbox fills, then the backlog, and
/// every event past that is accounted for as a counted whole-backlog drop.
#[test]
fn soak_a_stalled_client_drops_in_counted_whole_backlogs() {
    let mut outbox = Outbox::new();
    let mut inbox = Inbox::new(64);
    let mut dropped = 0usize;
    let total = 100_000u32;
    for item in 0..total {
        if let Some(Pushed::Dropped { lost }) = deliver(&mut outbox, &mut inbox, item) {
            assert_eq!(lost, CAPACITY + 1);
            dropped += lost;
            outbox.resynced();
        }
    }
    assert_eq!(64 + outbox.len() + dropped, total as usize);
    assert!(dropped > 0);
}
