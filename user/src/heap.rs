//! A bump allocator backed by the `sbrk` syscall.
//!
//! Growth is monotonic (nothing is ever freed) — enough for a short-lived
//! interpreter and far simpler than a full allocator. A fresh `CHUNK` is
//! requested from the kernel whenever the current one cannot satisfy a request.

use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

/// Bytes requested from the kernel at a time.
const CHUNK: usize = 64 * 1024;

struct Bump {
    next: usize,
    end: usize,
}

/// A global allocator; `UnsafeCell` because allocation mutates it through `&self`.
struct Allocator(UnsafeCell<Bump>);

// Safety: LazyOS user programs are single-threaded.
unsafe impl Sync for Allocator {}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator(UnsafeCell::new(Bump { next: 0, end: 0 }));

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

impl Bump {
    fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(8);
        let mut address = align_up(self.next, align);

        if address + layout.size() > self.end {
            // Not enough room: ask the kernel for a fresh, generously-sized chunk.
            let need = (layout.size() + align + CHUNK).max(CHUNK);
            let base = sys::sbrk(need as u64);
            if base == u64::MAX {
                return core::ptr::null_mut();
            }
            self.next = base as usize;
            self.end = base as usize + need;
            address = align_up(self.next, align);
        }

        self.next = address + layout.size();
        address as *mut u8
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        (&mut *self.0.get()).allocate(layout)
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator: memory is reclaimed only when the program exits.
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
