//! The key-down bitmap a focused session can poll (`docs/input-plan.md`, I3).
//!
//! A game asks "is W held?" every frame. Events answer it only after a
//! Messenger round trip and a queue drain; this page answers it with one
//! memory read. The client creates a shared buffer and hands it to `inputd`
//! (`os.lazy.input.v1` `AttachKeyState`); `inputd` writes the keys held right
//! now into it while the session has keyboard focus, and clears it (and the
//! focused flag) the moment focus leaves, before `KeyboardLeave` is sent.
//! Events still flow for edges; the bitmap is the authority on what is held.
//!
//! **Layout** ([`SharedKeys`], 56 bytes at the start of the buffer, all
//! little-endian `u64`): a seqlock word (odd while `inputd` writes), the raw
//! sequence number of the newest edge reflected, flags ([`FOCUSED`]), and 256
//! bits, one per HID usage on page 0x07 (bit `u & 63` of word `u >> 6`).
//!
//! **Trust.** The buffer is the client's, and every shared-buffer mapping is
//! writable on both sides. `inputd` therefore only ever *writes* it:
//! the seqlock counter it stores is its own, never read back, so a client
//! scribbling over the page can confuse nobody but itself. The page carries
//! only what that session's `KeyEvent`s already told it, and nothing while it
//! is not focused, so it widens no one's view of the keyboard.

use core::sync::atomic::{fence, AtomicU64, Ordering};

/// `flags` bit: the session has keyboard focus; the bitmap is live.
pub const FOCUSED: u64 = 1;

/// Bytes [`SharedKeys`] occupies; any shared buffer (a page at least) holds it.
pub const SIZE: usize = core::mem::size_of::<SharedKeys>();

/// Reads a reader retries a torn snapshot before giving up for this frame.
const READ_ATTEMPTS: usize = 64;

/// The shared page, as both sides see it.
#[repr(C)]
pub struct SharedKeys {
    /// Seqlock: odd while a write is in progress, bumped by two per write.
    pub lock: AtomicU64,
    /// The raw sequence number of the newest key edge the bitmap reflects.
    pub seq: AtomicU64,
    /// [`FOCUSED`].
    pub flags: AtomicU64,
    /// One bit per HID usage 0..=255.
    pub down: [AtomicU64; 4],
}

/// A consistent copy of the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub seq: u64,
    pub focused: bool,
    pub down: [u64; 4],
}

impl Snapshot {
    /// Whether HID usage `usage` is held (always `false` while unfocused).
    pub fn is_down(&self, usage: u16) -> bool {
        let (word, mask) = bit(usage);
        self.focused && self.down[word] & mask != 0
    }
}

fn bit(usage: u16) -> (usize, u64) {
    ((usage as usize >> 6) & 3, 1u64 << (usage & 63))
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
    pub fn publish(&mut self, page: &SharedKeys, state: Snapshot) -> bool {
        let state = if state.focused {
            state
        } else {
            // Unfocused, the page says nothing about the keyboard.
            Snapshot {
                down: [0; 4],
                ..state
            }
        };
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
        for (word, bits) in page.down.iter().zip(state.down) {
            word.store(bits, Ordering::Relaxed);
        }
        page.lock.store(
            self.generation.wrapping_mul(2).wrapping_add(2),
            Ordering::Release,
        );
        self.last = Some(state);
        true
    }
}

impl SharedKeys {
    /// A zeroed page (what a fresh shared buffer holds).
    pub const fn new() -> SharedKeys {
        SharedKeys {
            lock: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            flags: AtomicU64::new(0),
            down: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
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
            let mut down = [0u64; 4];
            for (out, word) in down.iter_mut().zip(&self.down) {
                *out = word.load(Ordering::Relaxed);
            }
            fence(Ordering::Acquire);
            if self.lock.load(Ordering::Relaxed) == before {
                return Some(Snapshot {
                    seq,
                    focused: flags & FOCUSED != 0,
                    down,
                });
            }
        }
        None
    }
}

impl Default for SharedKeys {
    fn default() -> SharedKeys {
        SharedKeys::new()
    }
}

/// A bitmap with exactly `usages` set (tests and `inputd`'s engine view).
pub fn bits_of(usages: &[u16]) -> [u64; 4] {
    let mut down = [0u64; 4];
    for &usage in usages {
        let (word, mask) = bit(usage);
        down[word] |= mask;
    }
    down
}
