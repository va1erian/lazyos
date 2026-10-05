//! The physical frame allocator's reference counts, free chain and counters.
//!
//! Every frame has a `u32` reference count: `0` free, `1..` live (more than
//! one when address spaces share it copy-on-write), and [`RESERVED`] for
//! frames the allocator owns itself or keeps out of general use. Freed frames
//! are linked into a chain through their own first word. Both live in
//! physical memory, which this module reaches only through [`FrameMemory`].

/// Physical frame size: the unit of allocation and refcounting.
pub const FRAME_SIZE: u64 = 4096;
/// Refcount of a frame the allocator must never hand out or free.
pub const RESERVED: u32 = u32::MAX;
/// The empty chain's head. Physical address 0 is never a usable frame, so it
/// is a safe sentinel.
pub const FREE_LIST_END: u64 = 0;

/// The memory the bookkeeping lives in: one refcount per frame, and a link
/// word at the start of each free frame. Addresses are physical and frame
/// aligned; implementations may assume the [`Ledger`] only passes frames it
/// was told are usable.
pub trait FrameMemory {
    fn refcount(&self, phys: u64) -> u32;
    fn set_refcount(&mut self, phys: u64, value: u32);
    /// The chain link stored in the free frame at `phys`.
    fn link(&self, phys: u64) -> u64;
    fn set_link(&mut self, phys: u64, next: u64);
}

/// Why a share or a release was refused. Nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not frame aligned, or outside every usable region.
    Unusable,
    /// Released at refcount zero: a double free.
    DoubleFree,
    /// The frame is [`RESERVED`].
    Reserved,
    /// Shared while free, reserved, or at the counter's limit.
    NotLive(u32),
}

/// What dropping one reference did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// The last reference: the frame is on the free chain again.
    Freed,
    /// The last reference to a frame of a separately managed pool: it is
    /// marked [`RESERVED`] and the caller returns it to that pool.
    PoolFreed,
    /// Others still hold the frame; this many references remain.
    Shared(u32),
    /// Refused; nothing changed.
    Invalid(Refused),
}

/// The count after one more reference, or why there may not be one.
pub fn shared(count: u32) -> Result<u32, Refused> {
    if count == 0 || count >= RESERVED - 1 {
        return Err(Refused::NotLive(count));
    }
    Ok(count + 1)
}

/// The count after one reference fewer, or why it cannot drop.
pub fn dropped(count: u32) -> Result<u32, Refused> {
    match count {
        0 => Err(Refused::DoubleFree),
        RESERVED => Err(Refused::Reserved),
        live => Ok(live - 1),
    }
}

fn check_usable(phys: u64, usable: bool) -> Result<(), Refused> {
    if phys & (FRAME_SIZE - 1) != 0 || !usable {
        return Err(Refused::Unusable);
    }
    Ok(())
}

/// The free chain plus the allocator's counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ledger {
    /// First frame of the free chain ([`FREE_LIST_END`] when empty).
    head: u64,
    /// Cumulative allocations.
    pub allocated: usize,
    /// Cumulative frees that put a frame back on the chain.
    pub freed: usize,
    /// Releases of a free frame (a bug indicator).
    pub double_frees: usize,
    /// Releases of an unusable or reserved frame (a bug indicator).
    pub invalid_frees: usize,
}

impl Ledger {
    pub const fn new() -> Self {
        Ledger {
            head: FREE_LIST_END,
            allocated: 0,
            freed: 0,
            double_frees: 0,
            invalid_frees: 0,
        }
    }

    /// Whether the chain holds a frame.
    pub fn has_free(&self) -> bool {
        self.head != FREE_LIST_END
    }

    /// Frames handed out and not yet returned (the leak report).
    pub fn live(&self) -> usize {
        self.allocated - self.freed
    }

    /// Link the free frame `phys` at the head of the chain.
    pub fn push_free(&mut self, mem: &mut impl FrameMemory, phys: u64) {
        mem.set_link(phys, self.head);
        self.head = phys;
    }

    /// Unlink the head of the chain.
    pub fn pop_free(&mut self, mem: &impl FrameMemory) -> Option<u64> {
        if self.head == FREE_LIST_END {
            return None;
        }
        let phys = self.head;
        self.head = mem.link(phys);
        Some(phys)
    }

    /// Record that `phys` was just handed out with one reference.
    pub fn note_alloc(&mut self, mem: &mut impl FrameMemory, phys: u64) {
        mem.set_refcount(phys, 1);
        self.allocated += 1;
    }

    /// Add a reference to the live frame `phys` (`usable`: it lies in a
    /// usable region).
    pub fn share(
        &self,
        mem: &mut impl FrameMemory,
        phys: u64,
        usable: bool,
    ) -> Result<(), Refused> {
        check_usable(phys, usable)?;
        let next = shared(mem.refcount(phys))?;
        mem.set_refcount(phys, next);
        Ok(())
    }

    /// Drop one reference to `phys`. At zero a general frame goes back on the
    /// chain; a frame of a separately managed pool (`in_pool`) is marked
    /// [`RESERVED`] for its pool, and stays out of the counters, so pool
    /// traffic never moves [`Ledger::live`].
    pub fn release(
        &mut self,
        mem: &mut impl FrameMemory,
        phys: u64,
        usable: bool,
        in_pool: bool,
    ) -> Release {
        if let Err(why) = check_usable(phys, usable) {
            self.invalid_frees += 1;
            return Release::Invalid(why);
        }
        let remaining = match dropped(mem.refcount(phys)) {
            Ok(remaining) => remaining,
            Err(why) => {
                match why {
                    Refused::DoubleFree => self.double_frees += 1,
                    _ => self.invalid_frees += 1,
                }
                return Release::Invalid(why);
            }
        };
        if remaining != 0 {
            mem.set_refcount(phys, remaining);
            return Release::Shared(remaining);
        }
        if in_pool {
            mem.set_refcount(phys, RESERVED);
            return Release::PoolFreed;
        }
        mem.set_refcount(phys, 0);
        self.push_free(mem, phys);
        self.freed += 1;
        Release::Freed
    }
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger::new()
    }
}
