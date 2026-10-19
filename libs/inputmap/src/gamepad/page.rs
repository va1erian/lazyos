//! The gamepad-state page a focused session can poll (`AttachGamepadState`,
//! `docs/usb-gamepad-plan.md` decision 5): `inputd` writes every attached pad's
//! current state into a shared buffer while the session has keyboard focus and
//! clears it, before `GamepadLeave`, the moment focus leaves. Same contract as
//! [`crate::keystate`], whose seqlock and trust rules this mirrors: the buffer
//! is the client's, so `inputd` only ever *writes* it and its own counter is
//! never read back.
//!
//! **Layout** ([`SharedPads`], 128 bytes at the start of the buffer, all
//! little-endian `u64`):
//!
//! | word | meaning |
//! |---|---|
//! | 0 | seqlock (odd while `inputd` writes) |
//! | 1 | raw `seq` of the newest record reflected |
//! | 2 | flags ([`FOCUSED`]) |
//! | 3 | pad count `0..=4` |
//! | 4 + 3 *n* | slot *n*: bits 0-7 device id, 8-15 hat (`0..=8`), 32-63 button bits (bit `usage - 1`) |
//! | 5 + 3 *n* | axes X, Y, Z, Rx as `i16`, X in the low 16 bits |
//! | 6 + 3 *n* | axes Ry, Rz, Slider, Dial |
//!
//! Only the first `count` slots are meaningful; the others are zero.

use core::sync::atomic::{fence, AtomicU64, Ordering};

use super::{Gamepad, PadState, AXES, HAT_CENTRED, MAX_PADS};

/// `flags` bit: the session has keyboard focus; the pads are live.
pub const FOCUSED: u64 = 1;

/// Bytes [`SharedPads`] occupies; any shared buffer (a page at least) holds it.
pub const SIZE: usize = core::mem::size_of::<SharedPads>();

/// Reads a reader retries a torn snapshot before giving up for this frame.
const READ_ATTEMPTS: usize = 64;

/// Words per pad slot.
const SLOT_WORDS: usize = 3;

/// The shared page, as both sides see it.
#[repr(C)]
pub struct SharedPads {
    /// Seqlock: odd while a write is in progress, bumped by two per write.
    pub lock: AtomicU64,
    /// The raw sequence number of the newest record the page reflects.
    pub seq: AtomicU64,
    /// [`FOCUSED`].
    pub flags: AtomicU64,
    /// Pads in `slots`.
    pub count: AtomicU64,
    /// Device and buttons, then eight `i16` axes, per pad.
    pub slots: [AtomicU64; MAX_PADS * SLOT_WORDS],
}

/// A consistent copy of the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub seq: u64,
    pub focused: bool,
    /// Pads present in `pads`, `0..=MAX_PADS`.
    pub count: usize,
    pub pads: [PadState; MAX_PADS],
}

impl Snapshot {
    /// What the page of a session says now: every attached pad if the session
    /// has focus, nothing otherwise.
    pub fn of(engine: &Gamepad, focused: bool) -> Snapshot {
        let mut pads = [PadState::default(); MAX_PADS];
        let mut count = 0;
        for (slot, pad) in pads.iter_mut().zip(engine.pads()) {
            *slot = *pad;
            count += 1;
        }
        Snapshot {
            seq: engine.last_seq(),
            focused,
            count,
            pads,
        }
    }

    /// The pads present (none while unfocused).
    pub fn pads(&self) -> &[PadState] {
        &self.pads[..self.count.min(MAX_PADS)]
    }

    /// The pad on `device`, if present.
    pub fn pad(&self, device: u8) -> Option<&PadState> {
        self.pads().iter().find(|pad| pad.device == device)
    }
}

/// One slot as three words.
fn encode(pad: &PadState) -> [u64; SLOT_WORDS] {
    let axes = |from: usize| {
        pad.axes[from..from + 4]
            .iter()
            .enumerate()
            .fold(0u64, |word, (i, &axis)| {
                word | u64::from(axis as u16) << (16 * i)
            })
    };
    [
        u64::from(pad.device) | u64::from(pad.hat) << 8 | u64::from(pad.buttons) << 32,
        axes(0),
        axes(AXES / 2),
    ]
}

fn decode(words: [u64; SLOT_WORDS]) -> PadState {
    let mut axes = [0i16; AXES];
    for (index, axis) in axes.iter_mut().enumerate() {
        let word = words[1 + index / 4];
        *axis = (word >> (16 * (index % 4))) as u16 as i16;
    }
    PadState {
        device: words[0] as u8,
        // A hat past 8 is no direction.
        hat: match (words[0] >> 8) as u8 {
            hat @ 0..=HAT_CENTRED => hat,
            _ => HAT_CENTRED,
        },
        buttons: (words[0] >> 32) as u32,
        axes,
    }
}

/// `inputd`'s side: the writer of one session's page. It keeps its own
/// seqlock counter so nothing it does depends on what the page holds.
#[derive(Debug, Default)]
pub struct Writer {
    generation: u64,
    /// What the page last said, so an unchanged state is not rewritten.
    last: Option<Snapshot>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer::default()
    }

    /// Publish `state` to `page` unless it already says exactly that.
    /// Returns whether it wrote.
    pub fn publish(&mut self, page: &SharedPads, state: Snapshot) -> bool {
        let state = normalised(state);
        if self.last == Some(state) {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        page.lock
            .store(self.generation.wrapping_mul(2) | 1, Ordering::Relaxed);
        fence(Ordering::Release);
        page.seq.store(state.seq, Ordering::Relaxed);
        page.flags
            .store(if state.focused { FOCUSED } else { 0 }, Ordering::Relaxed);
        page.count.store(state.count as u64, Ordering::Relaxed);
        for (index, words) in page.slots.chunks(SLOT_WORDS).enumerate() {
            let encoded = if index < state.count {
                encode(&state.pads[index])
            } else {
                [0; SLOT_WORDS]
            };
            for (word, value) in words.iter().zip(encoded) {
                word.store(value, Ordering::Relaxed);
            }
        }
        page.lock.store(
            self.generation.wrapping_mul(2).wrapping_add(2),
            Ordering::Release,
        );
        self.last = Some(state);
        true
    }
}

/// What a page can say: nothing while unfocused, at most [`MAX_PADS`] pads,
/// and unused slots at their default.
fn normalised(state: Snapshot) -> Snapshot {
    let count = if state.focused {
        state.count.min(MAX_PADS)
    } else {
        0
    };
    let mut pads = [PadState::default(); MAX_PADS];
    pads[..count].copy_from_slice(&state.pads[..count]);
    Snapshot {
        seq: state.seq,
        focused: state.focused,
        count,
        pads,
    }
}

impl SharedPads {
    /// A zeroed page (what a fresh shared buffer holds).
    pub const fn new() -> SharedPads {
        SharedPads {
            lock: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            flags: AtomicU64::new(0),
            count: AtomicU64::new(0),
            slots: [const { AtomicU64::new(0) }; MAX_PADS * SLOT_WORDS],
        }
    }

    /// The client's read: a consistent snapshot, or `None` when every
    /// attempt raced a write (try again next frame).
    pub fn snapshot(&self) -> Option<Snapshot> {
        for _ in 0..READ_ATTEMPTS {
            let before = self.lock.load(Ordering::Acquire);
            if before & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let seq = self.seq.load(Ordering::Relaxed);
            let flags = self.flags.load(Ordering::Relaxed);
            let count = self.count.load(Ordering::Relaxed);
            let mut words = [0u64; MAX_PADS * SLOT_WORDS];
            for (out, word) in words.iter_mut().zip(&self.slots) {
                *out = word.load(Ordering::Relaxed);
            }
            fence(Ordering::Acquire);
            if self.lock.load(Ordering::Relaxed) == before {
                let count = count.min(MAX_PADS as u64) as usize;
                let mut pads = [PadState::default(); MAX_PADS];
                for (pad, slot) in pads.iter_mut().zip(words.chunks(SLOT_WORDS)).take(count) {
                    *pad = decode([slot[0], slot[1], slot[2]]);
                }
                return Some(Snapshot {
                    seq,
                    focused: flags & FOCUSED != 0,
                    count,
                    pads,
                });
            }
        }
        None
    }
}

impl Default for SharedPads {
    fn default() -> SharedPads {
        SharedPads::new()
    }
}
