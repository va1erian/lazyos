//! The split virtqueue's bookkeeping: free descriptors, requests in flight,
//! submitting a request and reaping completions. Used under the device's
//! lock only; the waiters read the lock-free hints in [`Hints`].
//!
//! Up to [`MAX_INFLIGHT`] requests, from any number of callers, are in the
//! queue at once. Each owns a control block (header and status byte) and a
//! descriptor chain. Whoever holds the lock reaps every completion the device
//! posted, frees its descriptors at once and marks its request done; the
//! request's owner takes the result and frees the request slot.

use core::sync::atomic::{fence, AtomicU16, AtomicU32, AtomicU64, Ordering};

use super::plan::Plan;
use super::queue::{write_desc, ControlCell, Queue, MAX_QUEUE};

/// Requests in the queue at once.
pub const MAX_INFLIGHT: usize = 8;

// Descriptor flags (virtio 0.9.5).
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
/// `VRING_AVAIL_F_NO_INTERRUPT`: the driver polls, so the device need not
/// raise its (shared) interrupt line.
pub const AVAIL_NO_INTERRUPT: u16 = 1;
// Block request types.
const BLK_IN: u32 = 0;
const BLK_OUT: u32 = 1;

/// One request in the queue.
#[derive(Clone, Copy)]
pub struct Inflight {
    head: u16,
    pub write: bool,
    pub lba: u64,
    pub bytes: usize,
    /// The device answered; `status` is its status byte.
    pub done: bool,
    pub status: u8,
}

/// What waiters read without the lock: a waiter is worth waking when any of
/// these moved since it last looked.
pub struct Hints {
    /// Bit per request slot whose request completed and is not yet taken.
    pub done: AtomicU32,
    /// Bumped by a device reset: every request of an older epoch failed.
    pub epoch: AtomicU64,
    /// Bumped whenever a request slot is freed.
    pub released: AtomicU64,
    /// The used-ring index as of the last reap.
    pub reaped: AtomicU16,
}

impl Hints {
    pub const fn new() -> Hints {
        Hints {
            done: AtomicU32::new(0),
            epoch: AtomicU64::new(0),
            released: AtomicU64::new(0),
            reaped: AtomicU16::new(0),
        }
    }
}

/// Queue positions, free descriptors and requests in flight.
pub struct Ring {
    pub qsize: u16,
    pub avail_off: usize,
    pub used_off: usize,
    avail_idx: u16,
    used_idx: u16,
    free: [u16; MAX_QUEUE],
    free_len: usize,
    pub inflight: [Option<Inflight>; MAX_INFLIGHT],
}

impl Ring {
    /// A fresh queue of `qsize` descriptors, all free.
    pub fn new(qsize: u16, avail_off: usize, used_off: usize) -> Ring {
        let mut free = [0u16; MAX_QUEUE];
        for (index, slot) in free.iter_mut().enumerate().take(usize::from(qsize)) {
            *slot = index as u16;
        }
        Ring {
            qsize,
            avail_off,
            used_off,
            avail_idx: 0,
            used_idx: 0,
            free,
            free_len: usize::from(qsize),
            inflight: [None; MAX_INFLIGHT],
        }
    }

    /// A free request slot, if any.
    pub fn free_slot(&self) -> Option<usize> {
        self.inflight.iter().position(Option::is_none)
    }

    /// Whether `plan` fits in the free descriptors (plus header and status).
    pub fn fits(&self, plan: &Plan) -> bool {
        self.free_len >= plan.count + 2
    }

    /// Put `plan` in the queue as request `slot` (free, and `fits`), using
    /// control block `control` at `control_phys`. The caller notifies.
    ///
    /// # Safety
    /// `queue` is this ring's queue memory and `control` is slot `slot`'s
    /// block; the caller holds the device lock and the plan's buffers stay
    /// valid until the request completes or the device is reset.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn submit(
        &mut self,
        queue: &Queue,
        control: &ControlCell,
        control_phys: u64,
        slot: usize,
        write: bool,
        lba: u64,
        plan: &Plan,
    ) {
        // SAFETY: the control block is this slot's and the slot is free, so
        // nothing (device or driver) uses it; the lock is held.
        unsafe {
            let block = control.0.get();
            let header = (*block).header.as_mut_ptr();
            (header as *mut u32).write_volatile(if write { BLK_OUT } else { BLK_IN });
            (header.add(4) as *mut u32).write_volatile(0);
            (header.add(8) as *mut u64).write_volatile(lba);
            // A non-zero sentinel tells "device answered" from "never touched".
            core::ptr::addr_of_mut!((*block).status).write_volatile(0xFF);
        }
        let base = queue.0.get() as *mut u8;
        let head = self.pop();
        let mut previous = head;
        // SAFETY: every descriptor index (here and below) came off the free
        // list, so it is below `qsize` (the table's length) and owned by
        // nobody, and the caller holds the device lock.
        unsafe { write_desc(base, usize::from(head), control_phys, 16, DESC_NEXT, 0) };
        let data_flags = if write {
            DESC_NEXT
        } else {
            DESC_NEXT | DESC_WRITE
        };
        for &(phys, len) in &plan.pieces[..plan.count] {
            let desc = self.pop();
            // SAFETY: as above; `previous` gets its `next` link now.
            unsafe {
                set_next(base, previous, desc);
                write_desc(base, usize::from(desc), phys, len, data_flags, 0);
            }
            previous = desc;
        }
        let status = self.pop();
        // SAFETY: as above.
        unsafe {
            set_next(base, previous, status);
            write_desc(
                base,
                usize::from(status),
                control_phys + 16,
                1,
                DESC_WRITE,
                0,
            );
        }
        // Publish the head in the available ring, then the index.
        let avail = (base as usize + self.avail_off) as *mut u8;
        let position = usize::from(self.avail_idx % self.qsize);
        // SAFETY: the available ring lies inside the queue memory at
        // `avail_off` with `qsize` entries (checked at attach).
        unsafe {
            (avail.add(4 + position * 2) as *mut u16).write_volatile(head);
            fence(Ordering::Release);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            (avail.add(2) as *mut u16).write_volatile(self.avail_idx);
        }
        self.inflight[slot] = Some(Inflight {
            head,
            write,
            lba,
            bytes: plan.bytes,
            done: false,
            status: 0,
        });
    }

    /// The device's used index right now (lock-free readers use this too).
    pub fn device_used(queue: &Queue, used_off: usize) -> u16 {
        fence(Ordering::Acquire);
        // SAFETY: the used index is a device-written u16 inside our queue.
        unsafe { ((queue.0.get() as *const u8).add(used_off + 2) as *const u16).read_volatile() }
    }

    /// Take every completion the device posted: free its descriptors and mark
    /// its request done (setting its bit in `hints.done`). Returns the
    /// `(write, bytes)` of each, for the counters, through `each`.
    pub fn reap(
        &mut self,
        queue: &Queue,
        controls: &[ControlCell; MAX_INFLIGHT],
        hints: &Hints,
        mut each: impl FnMut(bool, usize),
    ) {
        let base = queue.0.get() as *const u8;
        while Self::device_used(queue, self.used_off) != self.used_idx {
            let position = usize::from(self.used_idx % self.qsize);
            // SAFETY: the used ring lies inside the queue memory at
            // `used_off` with `qsize` 8-byte elements (checked at attach).
            let id = unsafe {
                (base.add(self.used_off + 4 + position * 8) as *const u32).read_volatile()
            };
            self.used_idx = self.used_idx.wrapping_add(1);
            let Some(slot) = self.inflight.iter().position(|entry| {
                entry.is_some_and(|entry| u32::from(entry.head) == id && !entry.done)
            }) else {
                continue; // nothing of ours: a device bug, not memory to free
            };
            // SAFETY: the device finished with this slot's status byte.
            let status =
                unsafe { core::ptr::addr_of!((*controls[slot].0.get()).status).read_volatile() };
            if let Some(entry) = self.inflight[slot].as_mut() {
                entry.done = true;
                entry.status = status;
                let (head, write, bytes) = (entry.head, entry.write, entry.bytes);
                self.free_chain(base, head);
                each(write, bytes);
            }
            hints.done.fetch_or(1 << slot, Ordering::AcqRel);
        }
        hints.reaped.store(self.used_idx, Ordering::Release);
    }

    /// Free request slot `slot` (its owner took the result).
    pub fn release(&mut self, slot: usize, hints: &Hints) {
        self.inflight[slot] = None;
        hints.done.fetch_and(!(1 << slot), Ordering::AcqRel);
        hints.released.fetch_add(1, Ordering::AcqRel);
    }

    /// Return the chain starting at `head` to the free list.
    fn free_chain(&mut self, base: *const u8, head: u16) {
        let mut desc = head;
        for _ in 0..self.qsize {
            // SAFETY: `desc` is a descriptor index of a chain this ring built,
            // below `qsize`; the device is done with it.
            let (flags, next) = unsafe {
                let entry = base.add(usize::from(desc) * 16);
                (
                    (entry.add(12) as *const u16).read_volatile(),
                    (entry.add(14) as *const u16).read_volatile(),
                )
            };
            self.free[self.free_len] = desc;
            self.free_len += 1;
            if flags & DESC_NEXT == 0 {
                return;
            }
            desc = next;
        }
    }

    fn pop(&mut self) -> u16 {
        self.free_len -= 1;
        self.free[self.free_len]
    }
}

/// Point descriptor `index`'s `next` at `next`.
///
/// # Safety
/// `base` is the descriptor table and `index` is below its length.
unsafe fn set_next(base: *mut u8, index: u16, next: u16) {
    // SAFETY: per the contract, the descriptor lies inside the table.
    unsafe { (base.add(usize::from(index) * 16 + 14) as *mut u16).write_volatile(next) };
}
