//! `demo=1`: the evidence clients, run through the mixer once a card is
//! attached (they used to be `sndd`'s; applications now reach the card only
//! through `audiod`).
//!
//! The run is a list of steps; the clients of one step start together (so
//! their streams really are mixed) and the next step waits until all of them
//! have exited. The host judges the recording (`tools/sound/run.py`), the
//! markers only say when the guest is done.

use alloc::format;
use alloc::vec::Vec;

use user::sys;

/// One client: the program and its arguments (`argv[1..]`).
type Client = (&'static str, &'static [&'static str]);
type Step = &'static [Client];

/// The default run: a real tone, the hostile-input probe, a stream
/// lifecycle soak of silence and the control-panel probe.
const DEFAULT_STEPS: &[Step] = &[
    &[(fhs::bin::BEEP, &["freq=880", "ms=800"])],
    &[(fhs::bin::BEEP, &["probe=1"])],
    &[(fhs::bin::BEEP, &["soak=40"])],
    &[(fhs::bin::MIXER, &["probe"])],
];

/// `LAZYOS_SOUND_MIX=1` (`tools/sound/run.py --mix`): two tones played at
/// once, which the recording must hold as one chord, then a tone at half
/// volume, which must measure about 6 dB below the driver's own.
const MIX_STEPS: &[Step] = &[
    &[
        (fhs::bin::BEEP, &["freq=660", "ms=1500"]),
        (fhs::bin::BEEP, &["freq=990", "ms=1500"]),
    ],
    &[(fhs::bin::BEEP, &["freq=880", "ms=800", "volume=50"])],
];

/// `LAZYOS_SOUND_MODPLAY=1` (`tools/sound/run.py --modplay`): the tracker
/// player's self-test melody.
const MODPLAY_STEPS: &[Step] = &[&[(fhs::bin::MODPLAY, &["selftest"])]];

const STEPS: &[Step] = if option_env!("LAZYOS_SOUND_MODPLAY").is_some() {
    MODPLAY_STEPS
} else if option_env!("LAZYOS_SOUND_MIX").is_some() {
    MIX_STEPS
} else {
    DEFAULT_STEPS
};

pub(super) struct Demo {
    next: usize,
    running: Vec<u64>,
}

impl Demo {
    /// The evidence run, or an empty one when `enabled` is false.
    pub(super) fn new(enabled: bool) -> Demo {
        Demo {
            next: if enabled { 0 } else { STEPS.len() },
            running: Vec::new(),
        }
    }

    /// Whether clients are running or still to start.
    pub(super) fn active(&self) -> bool {
        self.next < STEPS.len() || !self.running.is_empty()
    }

    /// Reap finished clients; start the next step once the previous one is
    /// done and a card is there to play on.
    pub(super) fn poll(&mut self, card_ready: bool) {
        while !self.running.is_empty() {
            let Some((pid, status)) = sys::wait(sys::clock()) else {
                break;
            };
            sys::write_str(&format!("AUDIOD:DEMO:EXIT pid={pid} status={status}\n"));
            self.running.retain(|&child| child != pid);
        }
        if !self.running.is_empty() || self.next >= STEPS.len() || !card_ready {
            return;
        }
        for &(program, args) in STEPS[self.next] {
            match sys::spawn_native(program, args) {
                Some(pid) => {
                    sys::write_str(&format!("AUDIOD:DEMO:SPAWN pid={pid}\n"));
                    self.running.push(pid);
                }
                None => {
                    // A client that cannot start ends the run: the harness
                    // then reports the missing marker instead of waiting.
                    sys::write_str("AUDIOD:DEMO:SPAWN failed (client ELF missing?)\n");
                    self.next = STEPS.len();
                    return;
                }
            }
        }
        self.next += 1;
    }
}
