//! The frame ring shared between a NIC driver and the network stack
//! (`docs/networking-plan.md` section 5, `os.lazy.net.nic.v1`).
//!
//! One ring carries Ethernet frames one way, from a single producer to a single
//! consumer, through a shared-memory region both sides mapped:
//!
//! ```text
//! offset 0          header page (4096 bytes)
//!   0x00 magic  0x04 version  0x08 slots  0x0C slot_bytes
//!   0x40 head   (producer index, free running u32)
//!   0x80 tail   (consumer index, free running u32)
//!   0xC0 armed  (consumer wants a wake-up, 0 or 1)
//! offset 4096       slot 0 .. slot N-1, SLOT_BYTES each: a u16 length, then the frame
//! ```
//!
//! Slots are fixed size on purpose. That costs memory (256 slots is 512 KiB)
//! but makes validation trivial and rules out the wrap-around bugs of a
//! variable-length byte ring.
//!
//! **The peer is hostile.** Each endpoint keeps its own index in private memory
//! and only *writes* it to the header; the peer's index and every slot length
//! are read from shared memory exactly once per use and validated before they
//! are used. A frame is copied out of the slot before the caller sees it, so the
//! producer rewriting the slot afterwards changes nothing the consumer parses.
//! An index that claims more frames than the ring holds *poisons* the
//! endpoint: every later call fails with `Corrupt`, and the owner is expected
//! to detach the peer. A peer can therefore corrupt or drop its own traffic,
//! but can never make this side read or write outside the region, panic or
//! loop.
//!
//! **Wake-up.** The ring has no blocking of its own. A consumer that is about to
//! sleep [`Consumer::arm`]s the ring and looks once more; a producer calls
//! [`Producer::take_notify`] after a burst and sends one message if it returns
//! `true`. The exchange in `take_notify` clears the flag, so a burst produces one
//! wake-up (`docs/networking-plan.md` section 5, wake-up).

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

mod consumer;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod producer;

use core::sync::atomic::AtomicU32;

pub use consumer::Consumer;
pub use producer::Producer;

/// Bytes of one slot: the `u16` length plus the frame.
pub const SLOT_BYTES: usize = 2048;
/// Bytes of the length prefix.
pub const LEN_BYTES: usize = 2;
/// Longest frame a slot holds.
pub const MAX_FRAME: usize = SLOT_BYTES - LEN_BYTES;
/// Bytes of the header page; the first slot starts here.
pub const HEADER_BYTES: usize = 4096;
/// Fewest slots a ring may have.
pub const MIN_SLOTS: u32 = 16;
/// Most slots a ring may have (2 MiB of slots).
pub const MAX_SLOTS: u32 = 1024;

/// `"FRNG"` little endian.
pub const MAGIC: u32 = 0x474E_5246;
/// Layout version; a ring of another version is refused at attach.
pub const VERSION: u32 = 1;

/// Header field offsets. Public so a test (or a debugger) can name the fields a
/// hostile peer would scribble on.
pub mod off {
    pub const MAGIC: usize = 0x00;
    pub const VERSION: usize = 0x04;
    pub const SLOTS: usize = 0x08;
    pub const SLOT_BYTES: usize = 0x0C;
    pub const HEAD: usize = 0x40;
    pub const TAIL: usize = 0x80;
    pub const ARMED: usize = 0xC0;
}

/// A frame buffer big enough for any slot, the type [`Consumer::pop`] fills.
pub type FrameBuf = [u8; MAX_FRAME];

/// Whether `slots` is a legal slot count: a power of two in
/// [`MIN_SLOTS`]..=[`MAX_SLOTS`].
pub const fn valid_slots(slots: u32) -> bool {
    slots >= MIN_SLOTS && slots <= MAX_SLOTS && slots.is_power_of_two()
}

/// Bytes of shared memory a ring of `slots` slots needs (0 for an illegal
/// count, so a caller cannot size a buffer for one).
pub const fn ring_bytes(slots: u32) -> usize {
    if valid_slots(slots) {
        HEADER_BYTES + slots as usize * SLOT_BYTES
    } else {
        0
    }
}

/// Why a ring could not be created or attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    /// The slot count is not a power of two in range.
    BadSlots,
    /// The region is not exactly [`ring_bytes`] long.
    BadLength,
    /// The region is not 4-byte aligned (real buffers are page aligned).
    Misaligned,
    /// The header does not describe the ring the attacher expects: wrong
    /// magic, version, slot geometry, or indices that are not zero.
    BadHeader,
}

/// A failure of a producer or consumer call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The peer's index claims more frames than the ring holds. The endpoint
    /// is poisoned; every later call returns this again.
    Corrupt,
}

/// Why [`Producer::push`] refused a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushError {
    /// Every slot is occupied; try again after the consumer drains.
    Full,
    /// A zero-length frame.
    Empty,
    /// Longer than [`MAX_FRAME`]; never truncated.
    TooLong,
    /// The endpoint is poisoned, see [`Error::Corrupt`].
    Corrupt,
}

/// The outcome of [`Consumer::pop`] when a slot was consumed but no frame was
/// delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopError {
    /// The slot claimed a length above [`MAX_FRAME`] (carried here). The slot
    /// was skipped, not truncated; the caller counts it as an oversize drop.
    BadLength(u16),
    /// The endpoint is poisoned, see [`Error::Corrupt`].
    Corrupt,
}

/// A view of the ring's shared memory. It only computes addresses; the
/// producer and consumer own the protocol.
#[derive(Clone, Copy)]
pub(crate) struct Region {
    base: *mut u8,
    slots: u32,
}

impl Region {
    pub(crate) fn mask(&self) -> u32 {
        self.slots - 1
    }

    pub(crate) fn slots(&self) -> u32 {
        self.slots
    }

    fn word(&self, offset: usize) -> &AtomicU32 {
        // SAFETY: `offset` is one of the header constants, inside the first
        // page and 4-byte aligned because `base` is (checked at construction).
        // The region stays mapped and is only accessed through atomics or
        // volatile operations for the endpoint's lifetime (constructor
        // contract).
        unsafe { AtomicU32::from_ptr(self.base.add(offset) as *mut u32) }
    }

    pub(crate) fn head(&self) -> &AtomicU32 {
        self.word(off::HEAD)
    }

    pub(crate) fn tail(&self) -> &AtomicU32 {
        self.word(off::TAIL)
    }

    pub(crate) fn armed(&self) -> &AtomicU32 {
        self.word(off::ARMED)
    }

    /// Pointer to the start of slot `index & mask`. In range by construction:
    /// the mask keeps the slot below `slots`, and the region is
    /// `ring_bytes(slots)` long.
    pub(crate) fn slot(&self, index: u32) -> *mut u8 {
        let slot = (index & self.mask()) as usize;
        // SAFETY: `slot < slots`, so the offset is inside the region.
        unsafe { self.base.add(HEADER_BYTES + slot * SLOT_BYTES) }
    }
}

/// One ring in shared memory, before it is split into its two endpoints. The
/// side that allocates the buffer calls [`Ring::create`]; the other calls
/// [`Ring::attach`], which trusts nothing in the header.
pub struct Ring {
    region: Region,
}

impl Ring {
    fn check(base: *mut u8, len: usize, slots: u32) -> Result<Region, InitError> {
        if !valid_slots(slots) {
            return Err(InitError::BadSlots);
        }
        if len != ring_bytes(slots) {
            return Err(InitError::BadLength);
        }
        if !(base as usize).is_multiple_of(4) {
            return Err(InitError::Misaligned);
        }
        Ok(Region { base, slots })
    }

    /// Initialize a fresh ring in `len` bytes at `base`, which must not be in
    /// use by a peer yet.
    ///
    /// # Safety
    /// `base` must be valid for reads and writes of `len` bytes and stay
    /// mapped while any endpoint made from this ring exists. The peer may
    /// modify the region at any time; this crate only accesses it through
    /// atomic and volatile operations and raw copies, but the caller must not
    /// create a Rust reference into it.
    pub unsafe fn create(base: *mut u8, len: usize, slots: u32) -> Result<Ring, InitError> {
        let region = Self::check(base, len, slots)?;
        let header = |offset: usize, value: u32| {
            // SAFETY: inside the header page of a region the caller vouches for.
            unsafe { core::ptr::write_volatile(base.add(offset) as *mut u32, value) }
        };
        header(off::MAGIC, MAGIC);
        header(off::VERSION, VERSION);
        header(off::SLOTS, slots);
        header(off::SLOT_BYTES, SLOT_BYTES as u32);
        header(off::HEAD, 0);
        header(off::TAIL, 0);
        header(off::ARMED, 0);
        Ok(Ring { region })
    }

    /// Attach to a ring the peer created, validating its header against the
    /// geometry the attacher expects (`slots`, from the request, not from the
    /// header). Both indices must be zero: a ring is used from birth, never
    /// resumed.
    ///
    /// # Safety
    /// As [`Ring::create`].
    pub unsafe fn attach(base: *mut u8, len: usize, slots: u32) -> Result<Ring, InitError> {
        let region = Self::check(base, len, slots)?;
        let read = |offset: usize| {
            // SAFETY: inside the header page of a region the caller vouches for.
            unsafe { core::ptr::read_volatile(base.add(offset) as *const u32) }
        };
        let header_ok = read(off::MAGIC) == MAGIC
            && read(off::VERSION) == VERSION
            && read(off::SLOTS) == slots
            && read(off::SLOT_BYTES) == SLOT_BYTES as u32
            && read(off::HEAD) == 0
            && read(off::TAIL) == 0
            && read(off::ARMED) <= 1;
        if !header_ok {
            return Err(InitError::BadHeader);
        }
        Ok(Ring { region })
    }

    /// The producing endpoint. Make at most one per ring.
    pub fn producer(&self) -> Producer {
        Producer::new(self.region, 0)
    }

    /// The consuming endpoint. Make at most one per ring.
    pub fn consumer(&self) -> Consumer {
        Consumer::new(self.region, 0)
    }

    pub fn slots(&self) -> u32 {
        self.region.slots
    }

    /// Endpoints that start at index `start` instead of 0, with the header
    /// indices set to match. Lets a test cross the `u32` wrap point without
    /// pushing four billion frames.
    #[cfg(any(test, feature = "fuzz"))]
    pub fn endpoints_at(&self, start: u32) -> (Producer, Consumer) {
        self.region
            .head()
            .store(start, core::sync::atomic::Ordering::SeqCst);
        self.region
            .tail()
            .store(start, core::sync::atomic::Ordering::SeqCst);
        (
            Producer::new(self.region, start),
            Consumer::new(self.region, start),
        )
    }
}

#[cfg(test)]
mod tests;
