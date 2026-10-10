//! DMA memory and bounce copies shared by the NVMe and AHCI drivers: static
//! pages the controller owns, and copies between a caller's segments and a
//! bounce page for buffers the controller cannot be pointed at directly.

use core::cell::UnsafeCell;

use x86_64::VirtAddr;

/// One 4 KiB page of DMA memory in the kernel image.
#[repr(C, align(4096))]
pub(super) struct Page(pub(super) UnsafeCell<[u8; 4096]>);

// SAFETY: a page is only touched by its controller's owner, under the
// device lock (or before the device is published, at attach).
unsafe impl Sync for Page {}

impl Page {
    pub(super) const fn new() -> Page {
        Page(UnsafeCell::new([0; 4096]))
    }

    pub(super) fn virt(&self) -> u64 {
        self.0.get() as u64
    }

    pub(super) fn phys(&self) -> Option<u64> {
        super::virt_to_phys(VirtAddr::new(self.virt())).map(|phys| phys.as_u64())
    }
}

/// Copy `len` bytes from `segments` starting `skip` bytes in, to `out`.
///
/// # Safety
/// Every segment must be readable for its length and `out` writable for
/// `len` bytes.
pub(super) unsafe fn gather(segments: &[(u64, usize)], skip: usize, out: *mut u8, len: usize) {
    walk(segments, skip, len, |address, offset, count| {
        // SAFETY: the caller's contract covers both ranges.
        unsafe { core::ptr::copy_nonoverlapping(address as *const u8, out.add(offset), count) };
    });
}

/// Copy `len` bytes from `src` into `segments` starting `skip` bytes in.
///
/// # Safety
/// Every segment must be writable for its length and `src` readable for
/// `len` bytes.
pub(super) unsafe fn scatter(segments: &[(u64, usize)], skip: usize, src: *const u8, len: usize) {
    walk(segments, skip, len, |address, offset, count| {
        // SAFETY: the caller's contract covers both ranges.
        unsafe { core::ptr::copy_nonoverlapping(src.add(offset), address as *mut u8, count) };
    });
}

/// Visit the pieces of `segments` covering bytes `skip..skip + len`, as
/// (address, offset into the range, count).
fn walk(
    segments: &[(u64, usize)],
    skip: usize,
    len: usize,
    mut visit: impl FnMut(u64, usize, usize),
) {
    let mut start = 0usize;
    let mut copied = 0usize;
    for &(address, seg_len) in segments {
        let end = start + seg_len;
        let want = skip + copied;
        if copied < len && want < end {
            let from = want - start;
            let count = (seg_len - from).min(len - copied);
            visit(address + from as u64, copied, count);
            copied += count;
        }
        start = end;
    }
}
