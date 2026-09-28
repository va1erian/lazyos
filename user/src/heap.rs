//! The user-space heap: size-class free lists over a bump region grown with
//! the `sbrk` syscall.
//!
//! Small requests (up to [`MAX_CLASS`] bytes) are rounded up to a power-of-two
//! size class; a freed block goes onto its class's intrusive free list and the
//! next request of that class reuses it. Long-running services (`sysmond`'s
//! periodic publisher, the compositor's per-event encodes, Messenger replies)
//! therefore reach a steady state instead of growing without bound. Requests
//! above [`MAX_CLASS`] (or with an alignment above [`MAX_ALIGN`]) are rare,
//! long-lived buffers: they come straight from the bump region and are only
//! reclaimed when the program exits. A fresh `CHUNK` is requested from the
//! kernel whenever the bump region cannot satisfy a request.

use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

/// Bytes requested from the kernel at a time.
const CHUNK: usize = 64 * 1024;
/// Smallest size class: room for the free-list link, and 16-byte aligned.
const MIN_CLASS_SHIFT: u32 = 4;
/// Largest size class that is recycled.
const MAX_CLASS_SHIFT: u32 = 16;
const MAX_CLASS: usize = 1 << MAX_CLASS_SHIFT;
/// Blocks of a class are aligned to the class size, capped here so a large
/// class wastes at most one page of padding in the bump region.
const MAX_ALIGN: usize = 4096;
const CLASSES: usize = (MAX_CLASS_SHIFT - MIN_CLASS_SHIFT + 1) as usize;

struct Heap {
    /// Bump region: `[next, end)` is untouched memory from the last chunk.
    next: usize,
    end: usize,
    /// Per-class free-list heads (`0` = empty); each free block's first word
    /// holds the next block's address.
    free: [usize; CLASSES],
}

/// A global allocator; `UnsafeCell` because allocation mutates it through `&self`.
struct Allocator(UnsafeCell<Heap>);

// Safety: LazyOS user programs are single-threaded.
unsafe impl Sync for Allocator {}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator(UnsafeCell::new(Heap {
    next: 0,
    end: 0,
    free: [0; CLASSES],
}));

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// The size-class index for `layout`, or `None` when it is served from the
/// bump region without recycling. Deterministic in `layout`, so `dealloc`
/// (which receives the allocation's layout) finds the same class.
fn class_of(layout: Layout) -> Option<usize> {
    let size = layout.size().max(layout.align()).max(1 << MIN_CLASS_SHIFT);
    if size > MAX_CLASS || layout.align() > MAX_ALIGN {
        return None;
    }
    let shift = usize::BITS - (size - 1).leading_zeros();
    Some((shift - MIN_CLASS_SHIFT) as usize)
}

/// Byte size of class `index`.
const fn class_size(index: usize) -> usize {
    1 << (index as u32 + MIN_CLASS_SHIFT)
}

impl Heap {
    /// Carve `size` bytes aligned to `align` from the bump region.
    fn bump(&mut self, size: usize, align: usize) -> *mut u8 {
        let align = align.max(8);
        let mut address = align_up(self.next, align);

        if self.next == 0 || address + size > self.end {
            // Not enough room: ask the kernel for a fresh, generously-sized chunk.
            let need = (size + align + CHUNK).max(CHUNK);
            let base = sys::sbrk(need as u64);
            if base == u64::MAX {
                return core::ptr::null_mut();
            }
            self.next = base as usize;
            self.end = base as usize + need;
            address = align_up(self.next, align);
        }

        self.next = address + size;
        address as *mut u8
    }

    fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let Some(class) = class_of(layout) else {
            return self.bump(layout.size(), layout.align());
        };
        let head = self.free[class];
        if head != 0 {
            // Safety: `head` is a block this heap handed out and got back; its
            // first word is the free-list link written by `release`.
            self.free[class] = unsafe { *(head as *const usize) };
            return head as *mut u8;
        }
        let size = class_size(class);
        self.bump(size, size.min(MAX_ALIGN))
    }

    fn release(&mut self, ptr: *mut u8, layout: Layout) {
        let Some(class) = class_of(layout) else {
            // Large or over-aligned blocks are not recycled.
            return;
        };
        // Safety: `ptr` is a live block of at least `class_size(class)` bytes
        // (>= 16) and 16-byte aligned, now owned by the heap again.
        unsafe { *(ptr as *mut usize) = self.free[class] };
        self.free[class] = ptr as usize;
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        (&mut *self.0.get()).allocate(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        (&mut *self.0.get()).release(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Growing or shrinking within one size class keeps the block.
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        if let (Some(old), Some(new)) = (class_of(layout), class_of(new_layout)) {
            if old == new {
                return ptr;
            }
        }
        let heap = &mut *self.0.get();
        let fresh = heap.allocate(new_layout);
        if !fresh.is_null() {
            core::ptr::copy_nonoverlapping(ptr, fresh, layout.size().min(new_size));
            heap.release(ptr, layout);
        }
        fresh
    }
}

#[alloc_error_handler]
fn out_of_memory(layout: Layout) -> ! {
    // Report the failing request without allocating (`format!` would need the
    // allocator that just failed).
    sys::write_str("user: out of memory (request ");
    let mut digits = [0u8; 20];
    let mut value = layout.size();
    let mut len = 0;
    loop {
        digits[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let mut text = [0u8; 20];
    for i in 0..len {
        text[i] = digits[len - 1 - i];
    }
    sys::write(&text[..len]);
    sys::write_str(" bytes)\n");
    sys::exit(1)
}
