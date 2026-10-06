//! A split virtqueue (virtio 1.x, 2.7): descriptor table, available ring and
//! used ring in one caller-provided, physically contiguous block.
//!
//! The driver owns descriptors and the available ring; the device owns the
//! used ring. Everything the device writes (used index, element ids and
//! lengths) is untrusted: an id outside the queue or not in flight is reported
//! as [`Error::DeviceError`] instead of indexing with it.
//!
//! The free list lives in the struct, so the queue needs no allocator.

use core::ptr;
use core::sync::atomic::{fence, Ordering};

use crate::Error;

/// Largest queue this implementation drives. Bounds the in-struct free list
/// (768 bytes of bookkeeping at the cap). 256 is the default ring size of
/// `docs/driver-config-plan.md` and what QEMU's virtio-net offers; a device that
/// offers less is driven at its own size, and a setting above the cap is
/// clamped to it. (It was 64 while only virtio-sound used the crate.)
pub const MAX_QUEUE: usize = 256;

const DESC_SIZE: usize = 16;
/// Descriptor flag: `next` is valid.
const F_NEXT: u16 = 1;
/// Descriptor flag: the device writes this buffer.
const F_WRITE: u16 = 2;

/// Byte layout of a queue of a given size inside its block. Offsets are from
/// the start of the block; the used ring is 4-byte aligned as the spec asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub avail: usize,
    pub used: usize,
    pub total: usize,
}

impl Layout {
    pub const fn new(size: u16) -> Layout {
        let size = size as usize;
        let avail = size * DESC_SIZE;
        // flags, idx, ring[size], used_event
        let avail_end = avail + 2 + 2 + 2 * size + 2;
        let used = (avail_end + 3) & !3;
        // flags, idx, ring[size] of {id, len}, avail_event
        let total = used + 2 + 2 + 8 * size + 2;
        Layout { avail, used, total }
    }
}

/// One buffer of a request: a bus address, a length, and who writes it.
#[derive(Clone, Copy, Debug)]
pub struct Buf {
    pub bus: u64,
    pub len: u32,
    /// `true` when the device writes the buffer (a reply), `false` when it
    /// only reads it (a request or audio data going out).
    pub device_writes: bool,
}

/// A completed request: its head descriptor and the byte count the device wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Used {
    pub head: u16,
    pub len: u32,
}

pub struct Virtqueue {
    mem: *mut u8,
    bus: u64,
    size: u16,
    layout: Layout,
    avail_idx: u16,
    last_used: u16,
    free_head: u16,
    num_free: u16,
    /// Requests (chains) the device has not returned yet.
    chains_in_flight: u16,
    /// Free-list link of every descriptor; only meaningful while it is free.
    next_free: [u16; MAX_QUEUE],
    /// Descriptors a head owns (0 when the head is not in flight).
    chain_len: [u8; MAX_QUEUE],
}

// SAFETY: the queue owns its pointer for the life of the driver task; it is
// only used from one thread, and the raw pointer merely prevents an automatic
// `Send`, which a driver moving the queue between its own threads may need.
unsafe impl Send for Virtqueue {}

impl Virtqueue {
    /// Bytes of contiguous memory a queue of `size` entries needs.
    pub const fn bytes_needed(size: u16) -> usize {
        Layout::new(size).total
    }

    /// Build a queue over `mem`, zeroing it.
    ///
    /// `bus` is the address the device uses for the same memory.
    ///
    /// # Safety
    /// `mem` must be valid for reads and writes of [`Self::bytes_needed`]
    /// bytes, 4-byte aligned, and stay mapped for the queue's lifetime. The
    /// caller must not touch it except through the queue while the device is
    /// using it.
    pub unsafe fn new(mem: *mut u8, bus: u64, size: u16) -> Result<Virtqueue, Error> {
        if size == 0 || usize::from(size) > MAX_QUEUE || !size.is_power_of_two() {
            return Err(Error::BadQueue);
        }
        if !(mem as usize).is_multiple_of(4) || !bus.is_multiple_of(4) {
            return Err(Error::BadQueue);
        }
        let layout = Layout::new(size);
        // SAFETY: the caller guarantees `layout.total` writable bytes at `mem`.
        unsafe { ptr::write_bytes(mem, 0, layout.total) };
        let mut queue = Virtqueue {
            mem,
            bus,
            size,
            layout,
            avail_idx: 0,
            last_used: 0,
            free_head: 0,
            num_free: size,
            chains_in_flight: 0,
            next_free: [0; MAX_QUEUE],
            chain_len: [0; MAX_QUEUE],
        };
        for (index, link) in queue
            .next_free
            .iter_mut()
            .take(usize::from(size))
            .enumerate()
        {
            *link = (index as u16 + 1) % size;
        }
        Ok(queue)
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    pub fn num_free(&self) -> u16 {
        self.num_free
    }

    /// Descriptors owned by requests the device has not returned yet.
    pub fn in_flight(&self) -> u16 {
        self.size - self.num_free
    }

    pub fn desc_bus(&self) -> u64 {
        self.bus
    }

    pub fn avail_bus(&self) -> u64 {
        self.bus + self.layout.avail as u64
    }

    pub fn used_bus(&self) -> u64 {
        self.bus + self.layout.used as u64
    }

    fn desc_ptr(&self, index: u16) -> *mut u8 {
        // SAFETY: `index < size` at every call site, so the offset stays inside
        // the descriptor table of the block `new` was given.
        unsafe { self.mem.add(usize::from(index) * DESC_SIZE) }
    }

    fn avail_ptr(&self) -> *mut u8 {
        // SAFETY: `layout.avail` is inside the block.
        unsafe { self.mem.add(self.layout.avail) }
    }

    fn used_ptr(&self) -> *mut u8 {
        // SAFETY: `layout.used` is inside the block.
        unsafe { self.mem.add(self.layout.used) }
    }

    /// Queue a request made of `bufs` (device-readable buffers first, as the
    /// spec requires). Returns the head descriptor; call the transport's
    /// notify afterwards.
    pub fn add(&mut self, bufs: &[Buf]) -> Result<u16, Error> {
        if bufs.is_empty() || bufs.len() > usize::from(self.size) || bufs.len() > 255 {
            return Err(Error::BadRequest);
        }
        if bufs.len() > usize::from(self.num_free) {
            return Err(Error::QueueFull);
        }
        let head = self.free_head;
        let mut current = head;
        for (position, buf) in bufs.iter().enumerate() {
            let last = position + 1 == bufs.len();
            let next = self.next_free[usize::from(current)];
            let mut flags = if buf.device_writes { F_WRITE } else { 0 };
            if !last {
                flags |= F_NEXT;
            }
            let desc = self.desc_ptr(current);
            // SAFETY: `desc` points at a 16-byte descriptor inside the table.
            unsafe {
                ptr::write_volatile(desc as *mut u64, buf.bus.to_le());
                ptr::write_volatile(desc.add(8) as *mut u32, buf.len.to_le());
                ptr::write_volatile(desc.add(12) as *mut u16, flags.to_le());
                ptr::write_volatile(desc.add(14) as *mut u16, next.to_le());
            }
            if last {
                self.free_head = next;
            }
            current = next;
        }
        self.num_free -= bufs.len() as u16;
        self.chains_in_flight += 1;
        self.chain_len[usize::from(head)] = bufs.len() as u8;

        let slot = usize::from(self.avail_idx % self.size);
        let avail = self.avail_ptr();
        // SAFETY: ring entry `slot < size` and the idx field are in the block.
        unsafe {
            ptr::write_volatile(avail.add(4 + 2 * slot) as *mut u16, head.to_le());
        }
        // The entry must be visible before the index that publishes it.
        fence(Ordering::Release);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        // SAFETY: the available index lives at offset 2 of the avail ring.
        unsafe { ptr::write_volatile(avail.add(2) as *mut u16, self.avail_idx.to_le()) };
        fence(Ordering::SeqCst);
        Ok(head)
    }

    /// Take one completed request, if the device returned any.
    pub fn pop_used(&mut self) -> Result<Option<Used>, Error> {
        let used = self.used_ptr();
        // SAFETY: the used index lives at offset 2 of the used ring.
        let device_idx = u16::from_le(unsafe { ptr::read_volatile(used.add(2) as *const u16) });
        fence(Ordering::Acquire);
        if device_idx == self.last_used {
            return Ok(None);
        }
        // A device cannot have completed more requests than we submitted.
        if device_idx.wrapping_sub(self.last_used) > self.chains_in_flight {
            return Err(Error::DeviceError);
        }
        let slot = usize::from(self.last_used % self.size);
        // SAFETY: element `slot < size` of {id: u32, len: u32} is in the block.
        let (id, len) = unsafe {
            let element = used.add(4 + 8 * slot);
            (
                u32::from_le(ptr::read_volatile(element as *const u32)),
                u32::from_le(ptr::read_volatile(element.add(4) as *const u32)),
            )
        };
        let head = u16::try_from(id).map_err(|_| Error::DeviceError)?;
        if head >= self.size || self.chain_len[usize::from(head)] == 0 {
            return Err(Error::DeviceError);
        }
        self.release_chain(head);
        self.last_used = self.last_used.wrapping_add(1);
        Ok(Some(Used { head, len }))
    }

    /// Return a finished chain's descriptors to the free list.
    fn release_chain(&mut self, head: u16) {
        let count = u16::from(self.chain_len[usize::from(head)]);
        self.chain_len[usize::from(head)] = 0;
        // Walk to the chain's last descriptor through the descriptors' own
        // `next` fields, which only the driver ever wrote.
        let mut tail = head;
        for _ in 1..count {
            // SAFETY: `tail < size`; the `next` field is at offset 14.
            tail = u16::from_le(unsafe {
                ptr::read_volatile(self.desc_ptr(tail).add(14) as *const u16)
            }) % self.size;
        }
        // Splice the chain in front of the free list.
        self.next_free[usize::from(tail)] = self.free_head;
        self.free_head = head;
        self.num_free += count;
        self.chains_in_flight -= 1;
    }
}

#[cfg(test)]
mod tests;
