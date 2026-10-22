//! [`ReviewGate`]: the state behind "Run a script you did not write here?".
//!
//! The gate keeps a *list of arrivals*, one per paste, drop, opened file or
//! share that nobody has confirmed. Each arrival is the exact text that came
//! in (the bytes an edit inserted, not the buffer around them), and it stays
//! under review while the buffer still holds a **preserved run** of it:
//! [`RUN`] consecutive bytes of the arrival (the whole arrival when it is
//! shorter than that), anywhere in the buffer. Typed text around it, between
//! two arrivals, or inside one never counts for or against it; it is the
//! arrival's own bytes that are looked for.
//!
//! Why a preserved run and not "half of it is left": a run is what can still
//! *do* something. A hostile script padded with comment boilerplate looks
//! mostly boilerplate, so a fraction judged at the two ends (or of the whole
//! thing) clears when the user deletes the padding and leaves the one line
//! that matters. A statement that survives intact, `sys::init::shutdown();`
//! and anything of at least [`RUN`] bytes, keeps the gate armed.
//!
//! What is and is not guaranteed:
//!
//! * Guaranteed: while any [`RUN`] consecutive bytes of an unconfirmed arrival
//!   are in the buffer, a Run asks. Deleting around it, typing around it or
//!   inside it (while a [`RUN`]-byte piece stays unbroken), pasting the same
//!   text twice, and deleting what lies between two pastes do not clear it.
//!   The check is exact byte equality, not a diff, so it cannot be slid off
//!   the arrival by an ambiguous diff.
//! * Not guaranteed: a foreign statement shorter than [`RUN`] bytes survives
//!   unasked once the rest of its arrival is gone; so does one the user's own
//!   edits cut into pieces shorter than [`RUN`] (a one-character change in the
//!   middle of a short call). A script the user *assembles* from several short
//!   foreign fragments, each below the run length, is not followed either.
//!   The inverse is by design: a rewrite that happens to keep a [`RUN`]-byte
//!   phrase of the arrival (a common idiom) asks once more, which costs one
//!   click and never lets foreign text run unseen.
//! * Once an arrival has no run left it is dropped for good: putting its text
//!   back by typing is the user's, putting it back by Undo is the caller's
//!   business (`xui_playground::arrival::Known`).
//!
//! The cost is O(buffer) per edit and per arrival (a hash set of the
//! arrival's [`RUN`]-byte windows, built once); more than [`MAX_ARRIVALS`]
//! pending arrivals are merged into the last one.

use std::collections::HashSet;
use std::fmt;

use super::review::Provenance;

/// Consecutive bytes of an arrival that keep the gate armed: long enough that
/// a rewrite does not keep one by chance and that typed text rarely repeats
/// one, short enough that any statement with a path or an interface name in it
/// (`sys::init::shutdown();` is 22) survives whole.
pub const RUN: usize = 12;
/// How many unconfirmed arrivals are tracked one by one.
pub(super) const MAX_ARRIVALS: usize = 64;

/// How an arrival is looked for in a buffer.
#[derive(Clone)]
enum Index {
    /// An arrival shorter than [`RUN`]: all of it, contiguous.
    Whole,
    /// Every [`RUN`]-byte window of the arrival.
    Windows(HashSet<[u8; RUN]>),
}

/// One unconfirmed arrival: the bytes that came in, and how to find a run of
/// them in the buffer.
#[derive(Clone)]
struct Arrival {
    bytes: Vec<u8>,
    index: Index,
}

impl Arrival {
    fn new(bytes: Vec<u8>) -> Arrival {
        let index = if bytes.len() < RUN {
            Index::Whole
        } else {
            Index::Windows(
                bytes
                    .windows(RUN)
                    .filter_map(|w| <[u8; RUN]>::try_from(w).ok())
                    .collect(),
            )
        };
        Arrival { bytes, index }
    }

    /// Whether `text` still holds a preserved run of this arrival.
    fn survives(&self, text: &[u8]) -> bool {
        match &self.index {
            Index::Whole => text.windows(self.bytes.len()).any(|w| w == self.bytes),
            Index::Windows(set) => text
                .windows(RUN)
                .filter_map(|w| <&[u8; RUN]>::try_from(w).ok())
                .any(|w| set.contains(w)),
        }
    }
}

impl fmt::Debug for Arrival {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Arrival({} bytes)", self.bytes.len())
    }
}

/// Where a gate stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Nothing to ask: the text is the user's own, or shipped with the app.
    Clear,
    /// Foreign text nobody has confirmed yet.
    Pending,
    /// The user confirmed.
    Confirmed,
}

/// The edit from `old` to `new` as `(start, removed, inserted)`: the bytes at
/// `start` in `old` that `inserted` bytes replaced in `new`, cut on character
/// boundaries.
fn region(old: &str, new: &str) -> (usize, usize, usize) {
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while prefix > 0 && !(old.is_char_boundary(prefix) && new.is_char_boundary(prefix)) {
        prefix -= 1;
    }
    let room = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(room)
        .take_while(|(x, y)| x == y)
        .count();
    while suffix > 0
        && !(old.is_char_boundary(a.len() - suffix) && new.is_char_boundary(b.len() - suffix))
    {
        suffix -= 1;
    }
    (prefix, a.len() - suffix - prefix, b.len() - suffix - prefix)
}

/// The "confirm before the first Run" state of one editor buffer.
#[derive(Clone, Debug)]
pub struct ReviewGate {
    /// The origin of the latest arrival.
    provenance: Provenance,
    /// The arrivals still under review (non-empty exactly while pending).
    arrivals: Vec<Arrival>,
    state: State,
}

impl ReviewGate {
    /// A gate for `text` that arrived as `provenance`, all of it. Empty text
    /// has nothing to review.
    pub fn new(provenance: Provenance, text: &str) -> ReviewGate {
        let pending = provenance.needs_review() && !text.is_empty();
        ReviewGate {
            provenance,
            arrivals: if pending {
                vec![Arrival::new(text.as_bytes().to_vec())]
            } else {
                Vec::new()
            },
            state: if pending {
                State::Pending
            } else {
                State::Clear
            },
        }
    }

    /// New text replaced the buffer (a file opened, a share): `text` is the
    /// whole buffer now. Whatever was confirmed before no longer covers it.
    pub fn arrive(&mut self, provenance: Provenance, text: &str) {
        *self = ReviewGate::new(provenance, text);
    }

    /// Text arrived inside the buffer (a paste, a drop): the edit took `old`
    /// to `new`. Only what the edit inserted is the arrival, so text the user
    /// typed around it stays theirs and cannot be deleted to leave a script
    /// that was never asked about. An arrival that finds unconfirmed ones
    /// still in the buffer is *added* to them, each judged on its own bytes:
    /// typed text between two pastes is never swallowed into either.
    pub fn arrive_edit(&mut self, provenance: Provenance, old: &str, new: &str) {
        let (at, _, inserted) = region(old, new);
        if !provenance.needs_review() || inserted == 0 {
            // Nothing foreign came in: an edit like any other.
            return self.edited(new);
        }
        let piece = new.as_bytes()[at..at + inserted].to_vec();
        self.edited(new);
        if self.state != State::Pending {
            self.arrivals.clear();
        }
        if self.arrivals.len() >= MAX_ARRIVALS {
            // Too many to follow one by one: the last one takes the rest.
            if let Some(last) = self.arrivals.pop() {
                let mut bytes = last.bytes;
                bytes.extend_from_slice(&piece);
                self.arrivals.push(Arrival::new(bytes));
            }
        } else {
            self.arrivals.push(Arrival::new(piece));
        }
        self.provenance = provenance;
        self.state = State::Pending;
    }

    /// The origin of the text as the gate sees it now: the arrival's, or
    /// [`Provenance::Typed`] once the user has rewritten it.
    pub fn provenance(&self) -> Provenance {
        match self.state {
            State::Clear if self.provenance.needs_review() => Provenance::Typed,
            _ => self.provenance,
        }
    }

    /// Whether the next Run must ask first.
    pub fn needs_confirmation(&self) -> bool {
        self.state == State::Pending
    }

    /// The user answered Run: the buffer runs without asking again.
    pub fn confirm(&mut self) {
        if self.state == State::Pending {
            self.state = State::Confirmed;
            self.arrivals.clear();
        }
    }

    /// The user edited the buffer; `text` is what it holds now. An arrival
    /// with no preserved run left in it (rewritten, or deleted) is the user's;
    /// when none is left the gate clears.
    pub fn edited(&mut self, text: &str) {
        if self.state != State::Pending {
            return;
        }
        self.arrivals.retain(|a| a.survives(text.as_bytes()));
        if self.arrivals.is_empty() {
            self.state = State::Clear;
        }
    }
}
