//! Display device grant (issue #113).
//!
//! S4.4 of `docs/platform-plan.md` hands the framebuffer and the input devices
//! to a userspace compositor: one task *binds* the display, after which the
//! kernel multiplexer stops repainting and the compositor owns every pixel and
//! every input event. The kernel side is deliberately tiny:
//!
//! * [`bind`] creates one screen-sized [`ipc::shared`] buffer, maps it into the
//!   caller, and returns its handle, address and geometry. The compositor
//!   writes pixels there directly and calls `present` (op 3) to blit a damage
//!   rectangle onto the real framebuffer. (Direct scanout of the bootloader
//!   framebuffer's physical pages is not safe with the current allocator
//!   teardown path: those frames are outside the usable regions, so
//!   `free_user_table` would report them as invalid frees. A scanout mapping is
//!   the zero-copy follow-up, tracked under S8; the copy here is one memcpy per
//!   damage rectangle and the surface handoff between apps and the compositor
//!   is already zero-copy via shared buffers.)
//! * input events are pushed by the keyboard and mouse IRQ handlers into a
//!   bounded queue ([`push_key`], [`push_pointer_move`], [`push_pointer_button`],
//!   [`push_pointer_wheel`])
//!   and drained by the compositor with the `input_poll` op. No Messenger
//!   traffic is needed for input, which keeps the IRQ path allocation-free.
//! * when the owner exits without unbinding, [`bound`] reports false and
//!   `mux` resumes painting; the kernel mux is always the fallback.
//!
//! The native syscall is number 12 (`rax = 12`, `rdi = op`):
//!
//! ```text
//!   op 0 (bind):          rsi -> [width, height, stride, bpp, buffer, va, size]
//!   op 1 (unbind):        -
//!   op 2 (input_poll):    rsi -> event records, rdx = capacity in bytes -> count
//!   op 3 (present):       rsi = packed damage (x | y<<16 | w<<32 | h<<48)
//!   op 7 (set_layout):    rsi = byte order
//!   op 8 (native_layout): -> the order present copies as is
//! ```
//!
//! The shared buffers clients draw into are `messenger` ops
//! (`OP_BUFFER_CREATE`/`MAP`/`CLOSE`, `ipc::syscalls::bufop`); ops 4 to 6
//! once held them and are now refused.
//!
//! All ops return 0 or `-errno`. Every pointer argument is validated against the
//! caller's page tables (`user_ptr::try_*`) before the kernel touches it, so a
//! kernel address, an unmapped range or a read-only page is `-EFAULT`.
//!
//! `bind` is privileged: the display grant hands one task every pixel and every
//! keystroke, so the caller must hold `CAP_SYS_ADMIN` (the "mounts and driver
//! grants" capability). An unprivileged task gets `-EPERM`.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicUsize, Ordering};

use spin::Mutex;

use crate::ipc::{credentials, shared};
use crate::task;
use crate::user_ptr;

mod abi;
pub mod bochs;
mod input;
pub mod logical;
pub mod modecfg;
pub mod modeset;
mod order;
mod present;
mod screen;

pub use abi::*;
pub use input::{
    arm_key_doorbell, disarm_key_doorbell, push_event, push_key, push_pointer_button,
    push_pointer_move, push_pointer_wheel,
};
#[cfg(lazyos_tests)]
pub use present::{chunk_rows, damage_rows, test_hooks as present_hooks};
pub use screen::{init, init_requested};

/// Sentinel for "no compositor bound".
const NO_OWNER: usize = usize::MAX;

/// Largest number of input events the kernel queue retains; the oldest event is
/// dropped when the compositor falls behind.
const MAX_EVENTS: usize = 256;

/// Framebuffer geometry copied from the boot info at startup. `width` and
/// `height` are the *logical* screen ([`logical`]), which is what every
/// client is told and every screen buffer is sized from.
#[derive(Clone, Copy, Default)]
struct Screen {
    width: u64,
    height: u64,
    stride: u64,
    bytes_per_pixel: u64,
}

/// The bound display grant. The owner slot lives in [`OWNER`] separately, so
/// IRQ paths can read it without taking this lock.
struct Grant {
    /// Screen-buffer handle in the owner's table.
    handle: u64,
    /// Screen-buffer mapping address in the owner's address space.
    va: u64,
    width: u64,
    height: u64,
    size: u64,
}

static SCREEN: Mutex<Screen> = Mutex::new(Screen {
    width: 0,
    height: 0,
    stride: 0,
    bytes_per_pixel: 0,
});
/// The logical screen's place in the framebuffer, read by the blit paths.
static LOGICAL: Mutex<logical::Logical> = Mutex::new(logical::Logical {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
});
static GRANT: Mutex<Option<Grant>> = Mutex::new(None);
/// Owner slot, readable without the grant lock so IRQ handlers can cheaply ask
/// "is a compositor bound?".
static OWNER: AtomicUsize = AtomicUsize::new(NO_OWNER);
static EVENTS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());

/// The logical screen: where the desktop lives inside the framebuffer.
pub fn logical() -> logical::Logical {
    *LOGICAL.lock()
}

/// The screen's `(width, height)` in pixels: the firmware mode, or the one
/// `display.mode` switched to ([`modeset`]).
pub fn size() -> (usize, usize) {
    let screen = *SCREEN.lock();
    (screen.width as usize, screen.height as usize)
}

/// Bytes of one screen-sized RGBA surface (what `bind` allocates), for the
/// display-buffer limits (`crate::limits`). Zero before [`init`].
pub fn screen_bytes() -> u64 {
    let screen = *SCREEN.lock();
    screen.width * screen.height * 4
}

/// The recorded `(width, height, stride, bytes_per_pixel)`, so a test that
/// pretends to another screen can put the boot geometry back.
#[cfg(lazyos_tests)]
pub fn geometry_for_test() -> (usize, usize, usize, usize) {
    let screen = *SCREEN.lock();
    (
        screen.width as usize,
        screen.height as usize,
        screen.stride as usize,
        screen.bytes_per_pixel as usize,
    )
}

/// Whether a live compositor currently owns the display.
///
/// The kernel mux asks this every frame: a compositor that exits without
/// unbinding stops being an owner, so the mux's fallback painting resumes
/// without any teardown hook in the scheduler.
pub fn bound() -> bool {
    let owner = OWNER.load(Ordering::Relaxed);
    owner != NO_OWNER && task::live(owner)
}

/// The task slot holding the display grant (the compositor), if one is live.
/// `inputd` uses it to authenticate its shell client: the grant needs
/// `CAP_SYS_ADMIN`, so "holds the display grant" is a kernel-backed identity.
pub fn owner() -> Option<usize> {
    let owner = OWNER.load(Ordering::Relaxed);
    (owner != NO_OWNER && task::live(owner)).then_some(owner)
}

/// The screen-buffer handle `slot` holds as the bound compositor, if any.
///
/// `OP_BUFFER_CLOSE` refuses it: closing it would leave the grant pointing
/// at a freed mapping that `present` still blits from.
pub(crate) fn grant_handle_of(slot: usize) -> Option<u64> {
    if OWNER.load(Ordering::Relaxed) != slot {
        return None;
    }
    GRANT.lock().as_ref().map(|grant| grant.handle)
}

/// The syscall entry point (syscall 12); returns 0 or `-errno`.
pub fn dispatch(op: u64, a1: u64, a2: u64) -> u64 {
    match op {
        op::BIND => bind(a1),
        op::UNBIND => unbind(),
        op::INPUT_POLL => input_poll(a1, a2),
        op::PRESENT => {
            let started = crate::perf::rdtsc();
            let result = present::present(a1);
            crate::perf::presented(started);
            result
        }
        op::SET_LAYOUT => order::set(a1),
        op::NATIVE_LAYOUT => order::native(),
        _ => negative(errno::EINVAL),
    }
}

/// syscall 12 op 0: claim the display for the caller.
///
/// Binds the framebuffer to the calling task: the mux stops painting, the
/// kernel starts queueing input events for it, and a screen buffer is created
/// and mapped so it can draw. Only one compositor can hold the display at a
/// time; a second bind while a live owner exists is `-EBUSY`, and the kernel
/// task (the mux itself) is refused. An owner re-binding gets its current
/// geometry back, which keeps a restarted service idempotent.
fn bind(info_ptr: u64) -> u64 {
    if info_ptr == 0 {
        return negative(errno::EFAULT);
    }
    let me = task::current();
    if me == task::KERNEL_TASK {
        return negative(errno::EPERM);
    }
    // The grant is a driver grant: without this gate any uid could bind the
    // display and capture every keystroke and pixel of every other session.
    if !credentials::of(me).has_cap(credentials::CAP_SYS_ADMIN) {
        return negative(errno::EPERM);
    }
    let owner = OWNER.load(Ordering::Relaxed);
    if owner == me {
        return write_info(info_ptr);
    }
    // Refuse a bad info block *before* the grant exists, so a failed bind
    // leaves no half-claimed display behind.
    if user_ptr::try_copy_words(info_ptr, &[0u64; INFO_WORDS]).is_err() {
        return negative(errno::EFAULT);
    }
    if owner != NO_OWNER && task::live(owner) {
        return negative(errno::EBUSY);
    }
    // The previous owner is gone: drop its (stale) grant before reusing it.
    *GRANT.lock() = None;
    OWNER.store(NO_OWNER, Ordering::Relaxed);
    EVENTS.lock().clear();
    order::reset();

    let screen = *SCREEN.lock();
    if screen.width == 0 || screen.height == 0 {
        return negative(errno::ENOENT);
    }
    // The logical screen, never the mode: a 4K mode would not fit the cap.
    let size = screen.width * screen.height * 4;
    let handle = match shared::create(size) {
        Ok(handle) => handle,
        Err(error) => return shared_errno(error),
    };
    let va = match shared::map(handle) {
        Ok(va) => va,
        Err(error) => {
            shared::close(handle).ok();
            return shared_errno(error);
        }
    };
    *GRANT.lock() = Some(Grant {
        handle,
        va,
        width: screen.width,
        height: screen.height,
        size,
    });
    OWNER.store(me, Ordering::Relaxed);
    // The borders around a reduced logical screen belong to nobody: clear
    // whatever the boot console left there.
    crate::console::clear_outside_logical();

    // Seed the pointer so a compositor can draw its cursor before the first
    // mouse packet arrives.
    let mouse = crate::input::mouse::state();
    push_event(Event {
        kind: event::POINTER_MOVE,
        a: mouse.x,
        b: mouse.y,
        reserved: 0,
    });
    write_info(info_ptr)
}

/// Write the bind output block into validated user memory.
fn write_info(ptr: u64) -> u64 {
    let screen = *SCREEN.lock();
    let grant = GRANT.lock();
    let (handle, va, size) = match grant.as_ref() {
        Some(grant) => (grant.handle, grant.va, grant.size),
        None => (0, 0, 0),
    };
    let mut words = [0u64; INFO_WORDS];
    words[INFO_WIDTH] = screen.width;
    words[INFO_HEIGHT] = screen.height;
    words[INFO_STRIDE] = screen.stride;
    words[INFO_BPP] = screen.bytes_per_pixel;
    words[INFO_BUFFER] = handle;
    words[INFO_VA] = va;
    words[INFO_SIZE] = size;
    if user_ptr::try_copy_words(ptr, &words).is_err() {
        return negative(errno::EFAULT);
    }
    0
}

/// syscall 12 op 1: release the display; only its owner may.
fn unbind() -> u64 {
    let owner = OWNER.load(Ordering::Relaxed);
    if owner == NO_OWNER || owner != task::current() {
        return negative(errno::EPERM);
    }
    let grant = GRANT.lock().take();
    if let Some(grant) = grant {
        // Drops the mapping and the frames; the mux's next frame repaints the
        // whole screen, so the stale scanout does not linger.
        shared::close(grant.handle).ok();
    }
    OWNER.store(NO_OWNER, Ordering::Relaxed);
    EVENTS.lock().clear();
    task::NEEDS_REDRAW.store(true, Ordering::Relaxed);
    0
}

/// syscall 12 op 2: drain queued input events into the caller's buffer.
///
/// Copies whole [`EVENT_BYTES`]-sized records; the return value is the number
/// of events written. Only the bound compositor may poll: input belongs to it.
fn input_poll(ptr: u64, capacity: u64) -> u64 {
    let owner = OWNER.load(Ordering::Relaxed);
    if owner == NO_OWNER || owner != task::current() {
        return negative(errno::EPERM);
    }
    if ptr == 0 {
        return negative(errno::EFAULT);
    }
    let slots = (capacity / EVENT_BYTES as u64) as usize;
    // Encode without dequeuing, copy out through the validated path, and only
    // then drop the delivered events: a bad buffer must not lose input, and
    // the `EVENTS` lock is never held across a user-memory access.
    let (count, encoded) = {
        let events = EVENTS.lock();
        let count = events.len().min(slots);
        let mut encoded = alloc::vec::Vec::with_capacity(count * EVENT_BYTES);
        for event in events.iter().take(count) {
            encoded.extend_from_slice(&event.kind.to_le_bytes());
            encoded.extend_from_slice(&event.a.to_le_bytes());
            encoded.extend_from_slice(&event.b.to_le_bytes());
            encoded.extend_from_slice(&event.reserved.to_le_bytes());
        }
        (count, encoded)
    };
    if user_ptr::try_copy_to(ptr, &encoded).is_err() {
        return negative(errno::EFAULT);
    }
    let mut events = EVENTS.lock();
    let delivered = count.min(events.len());
    events.drain(..delivered);
    count as u64
}

/// Test-harness hook: forget any grant and drop queued events. The suite runs
/// without a scheduler, so there is nothing to close or wake.
#[cfg(lazyos_tests)]
pub fn reset() {
    *GRANT.lock() = None;
    OWNER.store(NO_OWNER, Ordering::Relaxed);
    input::KEY_WAITER.store(NO_OWNER, Ordering::Relaxed);
    EVENTS.lock().clear();
    order::reset();
}

/// Test-harness hook: make `slot` the display owner without a bind.
#[cfg(lazyos_tests)]
pub fn set_owner_for_test(slot: usize) {
    OWNER.store(slot, Ordering::Relaxed);
}

/// Test-harness hook: run `f` as if firmware had chosen a `width` x `height`
/// mode (the logical screen follows), then restore the real geometry.
#[cfg(lazyos_tests)]
pub fn with_mode_for_test<R>(width: usize, height: usize, f: impl FnOnce() -> R) -> R {
    let (screen, fitted) = (*SCREEN.lock(), *LOGICAL.lock());
    let (stride, bpp) = (screen.stride as usize, screen.bytes_per_pixel as usize);
    init(width, height, stride.max(width), bpp);
    let result = f();
    *SCREEN.lock() = screen;
    *LOGICAL.lock() = fitted;
    result
}
