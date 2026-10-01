//! Split-virtqueue memory and descriptor writes.

use core::cell::UnsafeCell;

/// Legacy virtqueue size: QEMU reports 256 descriptors for virtio-blk.
pub(super) const MAX_QUEUE: usize = 256;
/// The legacy spec requires the used ring 4096-aligned; 8192 = align(4096+518).
const MAX_USED_OFF: usize = 8192;
const MAX_USED_BYTES: usize = 6 + MAX_QUEUE * 8;
pub(super) const QUEUE_BYTES: usize = MAX_USED_OFF + MAX_USED_BYTES;

/// The split virtqueue and rings. `static` memory is one contiguous region of
/// the loaded kernel image, which is what the legacy queue-address register
/// (a physical page frame number) requires.
#[repr(C, align(4096))]
pub(super) struct Queue(pub(super) UnsafeCell<[u8; QUEUE_BYTES]>);

// Safety: every access goes through `VirtioBlk::state`, whose mutex is held
// from the first descriptor write until the completion status is read.
unsafe impl Sync for Queue {}

/// The request header (16 bytes) plus the status byte. Kept on one page so
/// both DMA targets are physically contiguous.
#[repr(C)]
pub(super) struct Control {
    pub(super) header: [u8; 16],
    pub(super) status: u8,
}

#[repr(C, align(64))]
pub(super) struct ControlCell(pub(super) UnsafeCell<Control>);

// Safety: like `Queue`, only touched under the driver's mutex.
unsafe impl Sync for ControlCell {}

/// Write descriptor `index`.
///
/// # Safety
/// `queue` must point at a descriptor table with more than `index` entries.
pub(super) unsafe fn write_desc(
    queue: *mut u8,
    index: usize,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let desc = queue.add(index * 16);
    (desc as *mut u64).write_volatile(addr);
    (desc.add(8) as *mut u32).write_volatile(len);
    (desc.add(12) as *mut u16).write_volatile(flags);
    (desc.add(14) as *mut u16).write_volatile(next);
}
