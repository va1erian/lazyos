//! Stream events (issue #453): exactly one `Underrun` per dry spell, one
//! `Drained` per completed drain, rate-limited `Period`s only while wanted,
//! and the driver's starvation edge.

use std::vec;
use std::vec::Vec;

use crate::events::{Event, Kind, Starvation, Watch, PERIOD_TICKS};
use crate::tests::{mixer, open_attached, VecRing, OWNER, PERIOD};
use crate::Mixer;

/// One mixer step as `audiod` takes it: mix a period when there is one, let
/// the card play it,
/// collect completed drains, then ask the watch.
struct Rig {
    mixer: Mixer<VecRing>,
    watch: Watch,
    card: u64,
    now: u64,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            mixer: mixer(),
            watch: Watch::new(),
            card: 0,
            now: 0,
        }
    }

    /// Like `audiod`'s card pump, it mixes only when some stream can fill a
    /// period: a lone dry stream is never mixed, and its underrun must still
    /// be seen.
    fn step(&mut self, periods: bool) -> Vec<Event> {
        let mut out = vec![0i16; 2 * PERIOD];
        self.card += PERIOD as u64;
        if self.mixer.wants_output() {
            self.mixer.mix(&mut out, self.card);
        }
        let mut drained = Vec::new();
        self.mixer.played(self.card, |id| drained.push(id));
        self.now += PERIOD_TICKS;
        let mut events = Vec::new();
        self.watch.step(
            self.mixer.statuses(),
            &drained,
            self.now,
            periods,
            &mut events,
        );
        events
    }
}

fn kinds(events: &[Event]) -> Vec<Kind> {
    events.iter().map(|event| event.kind).collect()
}

fn feed(rig: &mut Rig, id: u32, ring: &VecRing, from: u64, frames: usize) {
    ring.write(from, 2, &vec![1; 2 * frames]);
    rig.mixer
        .commit(id, OWNER, from + frames as u64, 0)
        .unwrap();
}

#[test]
fn a_starved_stream_reports_exactly_one_underrun_per_dry_spell() {
    let mut rig = Rig::new();
    let (id, ring, _) = open_attached(&mut rig.mixer, OWNER, 48000, 2);
    feed(&mut rig, id, &ring, 0, PERIOD);
    rig.mixer.start(id, OWNER, 0).unwrap();
    let mut all = Vec::new();
    for _ in 0..20 {
        all.extend(rig.step(false));
    }
    let underruns: Vec<&Event> = all.iter().filter(|e| e.kind == Kind::Underrun).collect();
    assert_eq!(underruns.len(), 1, "{all:?}");
    assert_eq!(underruns[0].stream, id);
    assert_eq!(underruns[0].frames, PERIOD as u64);
    // Fed again, then dry again: a second spell, a second event.
    feed(&mut rig, id, &ring, PERIOD as u64, PERIOD);
    let mut again = Vec::new();
    for _ in 0..20 {
        again.extend(rig.step(false));
    }
    assert_eq!(kinds(&again), [Kind::Underrun]);
}

#[test]
fn a_drain_reports_drained_once() {
    let mut rig = Rig::new();
    let (id, ring, _) = open_attached(&mut rig.mixer, OWNER, 48000, 2);
    feed(&mut rig, id, &ring, 0, PERIOD / 2);
    rig.mixer.start(id, OWNER, 0).unwrap();
    assert_eq!(rig.mixer.drain(id, OWNER, 0), Ok(false));
    let mut all = Vec::new();
    for _ in 0..10 {
        all.extend(rig.step(false));
    }
    assert_eq!(kinds(&all), [Kind::Drained]);
    assert_eq!(all[0].stream, id);
}

#[test]
fn periods_follow_the_position_only_while_wanted() {
    let mut rig = Rig::new();
    let (id, ring, ring_frames) = open_attached(&mut rig.mixer, OWNER, 48000, 2);
    let ring_frames = ring_frames as usize;
    feed(&mut rig, id, &ring, 0, ring_frames);
    rig.mixer.start(id, OWNER, 0).unwrap();
    // Nobody listens: nothing.
    assert!(rig.step(false).is_empty());
    // Someone listens: one Period per step (each step is PERIOD_TICKS long),
    // each at a later position.
    let first = rig.step(true);
    assert_eq!(kinds(&first), [Kind::Period]);
    let second = rig.step(true);
    assert_eq!(kinds(&second), [Kind::Period]);
    assert!(second[0].frames > first[0].frames);
}

#[test]
fn periods_are_rate_limited() {
    let mut watch = Watch::new();
    let mut rig = Rig::new();
    let (id, ring, ring_frames) = open_attached(&mut rig.mixer, OWNER, 48000, 2);
    feed(&mut rig, id, &ring, 0, ring_frames as usize);
    rig.mixer.start(id, OWNER, 0).unwrap();
    let mut out = vec![0i16; 2 * PERIOD];
    let mut events = Vec::new();
    for step in 0..PERIOD_TICKS {
        rig.mixer.mix(&mut out, (step + 1) * PERIOD as u64);
        rig.mixer.played((step + 1) * PERIOD as u64, |_| {});
        // Many looks inside one rate-limit window.
        watch.step(rig.mixer.statuses(), &[], 100 + step, true, &mut events);
    }
    assert_eq!(kinds(&events), [Kind::Period]);
}

#[test]
fn a_closed_stream_is_forgotten() {
    let mut rig = Rig::new();
    let (id, _ring, _) = open_attached(&mut rig.mixer, OWNER, 48000, 2);
    rig.step(true);
    rig.mixer.close(id, OWNER, 0).unwrap();
    assert!(rig.step(true).is_empty());
    // A drain reported for a stream that closed in the same step still counts.
    let mut events = Vec::new();
    rig.watch
        .step(rig.mixer.statuses(), &[id], 0, false, &mut events);
    assert_eq!(kinds(&events), [Kind::Drained]);
}

#[test]
fn kinds_have_the_idl_ordinals() {
    let all = [
        Kind::Underrun,
        Kind::Overrun,
        Kind::Drained,
        Kind::DeviceError,
        Kind::Period,
    ];
    for (ordinal, kind) in all.iter().enumerate() {
        assert_eq!(kind.ordinal(), ordinal as u32);
    }
}

#[test]
fn the_driver_reports_starvation_once_per_spell() {
    let mut watch = Starvation::default();
    assert!(
        !watch.observe(false, true),
        "a stopped stream never underruns"
    );
    assert!(watch.observe(true, true));
    for _ in 0..100 {
        assert!(!watch.observe(true, true));
    }
    assert!(!watch.observe(true, false));
    assert!(watch.observe(true, true));
    assert_eq!(watch.count, 2);
    watch.reset();
    assert!(watch.observe(true, true));
}

/// Thousands of random feeds, starves, drains and restarts: every underrun
/// the engine counts becomes exactly one event, never more.
#[test]
fn soak_underruns_match_the_engine_count() {
    let mut rig = Rig::new();
    let (id, ring, ring_frames) = open_attached(&mut rig.mixer, OWNER, 48000, 2);
    rig.mixer.start(id, OWNER, 0).unwrap();
    let mut written = 0u64;
    let mut reported = 0u32;
    let mut state = 0x853c_49e6_748f_ea9bu64;
    for _ in 0..5000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        if state.is_multiple_of(3) {
            let room = ring_frames - (written - rig.mixer.position(id, OWNER, 0).unwrap());
            let frames = (state as usize % (2 * PERIOD)).min(room as usize);
            feed(&mut rig, id, &ring, written, frames);
            written += frames as u64;
        }
        reported += rig
            .step(state.is_multiple_of(5))
            .iter()
            .filter(|e| e.kind == Kind::Underrun)
            .count() as u32;
        let counted = rig.mixer.statuses().next().unwrap().underruns;
        assert_eq!(reported, counted);
    }
    assert!(reported > 0, "the soak never starved the stream");
}
