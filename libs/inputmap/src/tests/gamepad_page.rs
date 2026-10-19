//! The gamepad-state page: round trip, focus rules, hostile contents and the
//! seqlock under a concurrent writer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::gamepad::page::{SharedPads, Snapshot, Writer, FOCUSED, SIZE};
use crate::gamepad::{PadState, HAT_CENTRED, MAX_PADS};

fn pad(device: u8, n: i16) -> PadState {
    PadState {
        device,
        buttons: 0x8000_0001 ^ u32::from(n as u16),
        hat: (n as u8) % 9,
        axes: [n, -n, n / 2, i16::MIN, i16::MAX, 0, n.wrapping_mul(3), -1],
    }
}

fn live(pads: &[PadState]) -> Snapshot {
    let mut all = [PadState::default(); MAX_PADS];
    all[..pads.len()].copy_from_slice(pads);
    Snapshot {
        seq: 41,
        focused: true,
        count: pads.len(),
        pads: all,
    }
}

#[test]
fn the_page_is_small_and_word_aligned() {
    assert_eq!(SIZE, 128);
    assert_eq!(core::mem::align_of::<SharedPads>(), 8);
}

#[test]
fn a_zeroed_page_reads_as_nothing() {
    let page = SharedPads::new();
    let snapshot = page.snapshot().unwrap();
    assert!(!snapshot.focused);
    assert!(snapshot.pads().is_empty());
}

#[test]
fn state_round_trips() {
    let page = SharedPads::new();
    let mut writer = Writer::new();
    let state = live(&[pad(3, 1234), pad(9, -20000), pad(0, 0), pad(255, 7)]);
    assert!(writer.publish(&page, state));
    let read = page.snapshot().unwrap();
    assert_eq!(read, state);
    assert_eq!(read.pads().len(), 4);
    assert_eq!(read.pad(9), Some(&pad(9, -20000)));
    assert_eq!(read.pad(8), None);
    assert_eq!(page.flags.load(Ordering::Relaxed), FOCUSED);
}

#[test]
fn an_unchanged_state_is_not_rewritten() {
    let page = SharedPads::new();
    let mut writer = Writer::new();
    let state = live(&[pad(3, 5)]);
    assert!(writer.publish(&page, state));
    let lock = page.lock.load(Ordering::Relaxed);
    assert!(!writer.publish(&page, state));
    assert_eq!(page.lock.load(Ordering::Relaxed), lock);
    assert!(writer.publish(&page, live(&[pad(3, 6)])));
    assert_eq!(page.lock.load(Ordering::Relaxed), lock + 2);
}

#[test]
fn unfocused_shows_no_pads() {
    let page = SharedPads::new();
    let mut writer = Writer::new();
    writer.publish(&page, live(&[pad(3, 5), pad(4, 6)]));
    let cleared = Snapshot {
        focused: false,
        ..live(&[pad(3, 5), pad(4, 6)])
    };
    assert!(writer.publish(&page, cleared));
    let read = page.snapshot().unwrap();
    assert!(!read.focused);
    assert_eq!(read.count, 0);
    assert_eq!(read.pads, [PadState::default(); MAX_PADS]);
    // The words really are zero, not merely hidden by the count.
    assert!(page
        .slots
        .iter()
        .all(|word| word.load(Ordering::Relaxed) == 0));
}

#[test]
fn a_shorter_list_zeroes_the_slots_it_drops() {
    let page = SharedPads::new();
    let mut writer = Writer::new();
    writer.publish(&page, live(&[pad(1, 1), pad(2, 2), pad(3, 3)]));
    writer.publish(&page, live(&[pad(2, 2)]));
    let read = page.snapshot().unwrap();
    assert_eq!(read.pads(), [pad(2, 2)]);
    assert!(page.slots[3..]
        .iter()
        .all(|w| w.load(Ordering::Relaxed) == 0));
}

#[test]
fn a_hostile_page_is_read_safely() {
    let page = SharedPads::new();
    page.count.store(u64::MAX, Ordering::Relaxed);
    page.slots[0].store(0xFFFF_FFFF_FFFF_FFFF, Ordering::Relaxed);
    let read = page.snapshot().unwrap();
    assert_eq!(read.count, MAX_PADS);
    assert_eq!(read.pads[0].hat, HAT_CENTRED);
    assert_eq!(read.pads[0].buttons, u32::MAX);
}

#[test]
fn a_count_beyond_the_slots_is_clamped_when_writing() {
    let page = SharedPads::new();
    let mut writer = Writer::new();
    let mut state = live(&[pad(1, 1)]);
    state.count = 99;
    writer.publish(&page, state);
    assert_eq!(page.snapshot().unwrap().count, MAX_PADS);
}

#[test]
fn a_reader_never_sees_a_torn_write() {
    let page = Arc::new(SharedPads::new());
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (page, stop) = (page.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut writer = Writer::new();
            let mut n: i16 = 1;
            while !stop.load(Ordering::Relaxed) {
                // Every field is a function of `n`, so any mixture of two
                // writes is detectable.
                let pads = [pad(1, n), pad(2, n), pad(3, n)];
                let mut state = live(&pads);
                state.seq = n as u64;
                writer.publish(&page, state);
                n = n.wrapping_add(1).max(1);
            }
        })
    };
    let (mut seen, mut spins) = (0u32, 0u32);
    while seen < 20_000 && spins < 5_000_000 {
        spins += 1;
        let Some(read) = page.snapshot() else {
            continue;
        };
        if read.count == 0 {
            continue;
        }
        let n = read.seq as i16;
        assert_eq!(
            read.pads(),
            [pad(1, n), pad(2, n), pad(3, n)],
            "torn at {n}"
        );
        seen += 1;
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    assert!(seen > 0, "the reader never got a snapshot");
}

#[test]
fn a_snapshot_of_the_engine_lists_its_pads_when_focused() {
    use crate::gamepad::{raw, Gamepad, RawPad};
    let mut engine = Gamepad::new();
    for device in [5, 2] {
        assert!(engine.attach(device));
    }
    let mut out = std::vec::Vec::new();
    let record = |device, kind, code, value| RawPad {
        seq: 9,
        ts_ns: 90,
        device,
        kind,
        code,
        value,
    };
    engine.apply(record(2, raw::BUTTON, 3, 1), &mut out);
    engine.apply(record(5, raw::AXIS, 0x30, 32767), &mut out);
    engine.flush(&mut out);
    let focused = Snapshot::of(&engine, true);
    assert_eq!((focused.seq, focused.count), (9, 2));
    assert_eq!(focused.pads()[0].device, 5, "slot order");
    assert_eq!(focused.pad(2).unwrap().buttons, 0b100);
    assert_eq!(focused.pad(5).unwrap().axes[0], 32767);
    // Through a page and back.
    let page = SharedPads::new();
    Writer::new().publish(&page, focused);
    assert_eq!(page.snapshot().unwrap(), focused);
    let unfocused = Snapshot::of(&engine, false);
    Writer::new().publish(&page, unfocused);
    assert_eq!(page.snapshot().unwrap().count, 0);
}

#[test]
fn an_unfocused_page_does_not_leak_the_newest_seq() {
    let page = SharedPads::new();
    let mut writer = Writer::new();
    assert!(writer.publish(&page, live(&[pad(3, 100)])));
    assert_eq!(page.snapshot().unwrap().seq, 41);
    // Focus leaves while the pad keeps moving: the page is empty, seq included.
    for newest in [42, 43, 9000] {
        let mut unfocused = live(&[pad(3, 100)]);
        unfocused.focused = false;
        unfocused.seq = newest;
        writer.publish(&page, unfocused);
        let read = page.snapshot().unwrap();
        assert_eq!((read.seq, read.count, read.focused), (0, 0, false));
        assert_eq!(page.seq.load(Ordering::Relaxed), 0);
    }
    // A pad moving while unfocused does not even cause a write.
    let mut quiet = live(&[pad(3, 1)]);
    quiet.focused = false;
    quiet.seq = 77;
    assert!(!writer.publish(&page, quiet));
}

#[test]
fn pad_traffic_goes_only_to_a_focused_session_that_opted_in() {
    use crate::gamepad::optin::OptIn;
    let mut optin = OptIn::new();
    // Nobody asked: nobody gets a thing, focused or not.
    assert_eq!(optin.recipient(Some(1)), None);
    assert_eq!(optin.recipient(None), None);
    optin.attach(2);
    optin.attach(2);
    assert_eq!(optin.len(), 1, "attaching twice is one opt-in");
    // Session 1 has focus but never opted in: a pad it ignores cannot fill its
    // endpoint. Session 2 opted in but is not focused: it sees nothing either.
    assert_eq!(optin.recipient(Some(1)), None);
    assert_eq!(optin.recipient(None), None);
    // Focus moves to 2.
    assert_eq!(optin.recipient(Some(2)), Some(2));
    // Closing the session ends the opt-in; a new session reusing the id has
    // to ask again.
    optin.forget(2);
    assert!(optin.is_empty());
    assert_eq!(optin.recipient(Some(2)), None);
    optin.forget(2);
}
