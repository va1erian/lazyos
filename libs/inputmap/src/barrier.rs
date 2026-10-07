//! Keys typed right after a click wait for the focus the click gives.
//!
//! `inputd` reads keys and pointer buttons from one raw bus, but which window
//! a press focuses is the compositor's decision: the press goes out as a
//! `PointerEvent` and the new focus comes back as `NoteFocus`. A key read in
//! between (a script or a fast typist clicks and types at once) would be
//! routed by the focus from *before* the click and reach the window the click
//! left. So after a press is forwarded, the [`Barrier`] holds the engine's
//! outputs, in order, until the compositor reports it handled that pointer
//! event (`NoteInputDone`, sent after the focus it led to), or until
//! [`HOLD_TICKS`] pass: a compositor that never answers costs a short delay,
//! never the keys.

use alloc::vec::Vec;

use crate::Output;

/// Longest hold, in PIT ticks (100 Hz). A compositor answers in a
/// millisecond or two, a frame later during an animation; this only bounds
/// one that is stuck or does not send the note.
pub const HOLD_TICKS: u64 = 25;

/// Most outputs held; past this the hold is given up (and the outputs
/// delivered) rather than growing without bound.
pub const MAX_HELD: usize = 512;

#[derive(Default)]
pub struct Barrier {
    /// The newest press's pointer sequence and the tick the hold ends.
    waiting: Option<(u64, u64)>,
    held: Vec<Output>,
}

impl Barrier {
    pub fn new() -> Barrier {
        Barrier::default()
    }

    /// Whether outputs are being held.
    pub fn holding(&self) -> bool {
        self.waiting.is_some()
    }

    /// A button press with pointer sequence `seq` went to the compositor at
    /// `now`: hold what follows until it is handled. A later press extends
    /// the hold to itself.
    pub fn pressed(&mut self, seq: u64, now: u64) {
        let seq = self.waiting.map_or(seq, |(old, _)| old.max(seq));
        self.waiting = Some((seq, now + HOLD_TICKS));
    }

    /// Hold `outputs` while waiting (they are moved out of the vector);
    /// otherwise leave them to be delivered. Returns what must be delivered
    /// first: the held outputs, when the bound was reached.
    pub fn hold(&mut self, outputs: &mut Vec<Output>) -> Vec<Output> {
        if self.waiting.is_none() {
            return Vec::new();
        }
        self.held.append(outputs);
        if self.held.len() >= MAX_HELD {
            return self.release();
        }
        Vec::new()
    }

    /// The compositor handled every pointer event up to `seq`: when that
    /// covers the press, the hold ends and the held outputs are returned to
    /// be delivered, in order, to the focus now in effect.
    pub fn settled(&mut self, seq: u64) -> Vec<Output> {
        match self.waiting {
            Some((press, _)) if seq >= press => self.release(),
            _ => Vec::new(),
        }
    }

    /// The hold timed out at `now`: give up waiting and return the outputs.
    pub fn expire(&mut self, now: u64) -> Vec<Output> {
        match self.waiting {
            Some((_, until)) if now >= until => self.release(),
            _ => Vec::new(),
        }
    }

    /// End the hold now (the compositor went away).
    pub fn release(&mut self) -> Vec<Output> {
        self.waiting = None;
        core::mem::take(&mut self.held)
    }

    /// The tick the hold ends at, so the service loop wakes for it.
    pub fn next_due(&self) -> Option<u64> {
        self.waiting.map(|(_, until)| until)
    }
}
