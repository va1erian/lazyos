//! Display device grant (issue #113).
//!
//! S4.4 of `docs/platform-plan.md` hands the framebuffer and the input devices
//! to a userspace compositor: one task *binds* the display, after which the
//! kernel multiplexer stops repainting and the compositor owns every pixel and
//! every input event. The kernel side is deliberately tiny:
//!
//! * [`bind`] creates one screen-sized [`ipc::shared`] buffer, maps it into the
//!   caller, and returns its handle, address and geometry. The compositor
//!   writes pixels there directly and calls [`present`] to blit a damage
//!   rectangle onto the real framebuffer. (Direct scanout of the bootloader
//!   framebuffer's physical pages is not safe with the current allocator
//!   teardown path: those frames are outside the usable regions, so
//!   `free_user_table` would report them as invalid frees. A scanout mapping is
//!   the zero-copy follow-up, tracked under S8; the copy here is one memcpy per
//!   damage rectangle and the surface handoff between apps and the compositor
//!   is already zero-copy via shared buffers.)
//! * input events are pushed by the keyboard and mouse IRQ handlers into a
//!   bounded queue ([`push_key`], [`push_pointer_move`], [`push_pointer_button`])
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
//!   op 4 (create_buffer): rsi = size, rdx -> [handle, va, size]
//!   op 5 (map_buffer):    rsi = handle, rdx -> va
//!   op 6 (close_buffer):  rsi = handle (unmap; -EBADF when not held)
//! ```
//!
//! All ops return 0 or `-errno`. Every pointer argument is validated against the
//! caller's page tables (`user_ptr::try_*`) before the kernel touches it, so a
//! kernel address, an unmapped range or a read-only page is `-EFAULT`.
//!
//! `bind` is privileged: the display grant hands one task every pixel and every
//! keystroke, so the caller must hold `CAP_SYS_ADMIN` (the "mounts and driver
//! grants" capability). An unprivileged task gets `-EPERM`; ops 4 and 5 (shared
//! surface buffers) stay open to every task.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicUsize, Ordering};

use spin::Mutex;

use crate::console;
use crate::input::keyboard::Key;
use crate::ipc::{credentials, shared};
use crate::task;
use crate::user_ptr;

mod buffers;

/// Sentinel for "no compositor bound".
const NO_OWNER: usize = usize::MAX;

/// Largest number of input events the kernel queue retains; the oldest event is
/// dropped when the compositor falls behind.
const MAX_EVENTS: usize = 256;

/// One input event, 16 bytes on the wire. `kind` is one of [`event`]; `a`/`b`
/// carry the payload (pointer position, button id, or key code).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Event {
    pub kind: u32,
    pub a: i32,
    pub b: i32,
    /// Padding; always zero. Keeps the record a multiple of eight bytes.
    pub reserved: u32,
}

/// Bytes per encoded [`Event`].
pub const EVENT_BYTES: usize = 16;

/// Input event kinds; mirrored by `user::messenger::display`.
pub mod event {
    /// Pointer moved: `a` = x, `b` = y (screen pixels).
    pub const POINTER_MOVE: u32 = 0;
    /// Pointer button pressed: `a` = button (see [`button`]).
    pub const POINTER_DOWN: u32 = 1;
    /// Pointer button released: `a` = button.
    pub const POINTER_UP: u32 = 2;
    /// Key pressed: `a` = key code (ASCII, control value, or [`key`] constant).
    pub const KEY_DOWN: u32 = 3;
    /// Key released: `a` = key code.
    pub const KEY_UP: u32 = 4;
}

/// Pointer buttons, as reported in `POINTER_DOWN`/`POINTER_UP`.
pub mod button {
    pub const LEFT: u32 = 1;
    pub const RIGHT: u32 = 2;
    pub const MIDDLE: u32 = 3;
}

/// Key codes for keys that are not a plain character. Printable keys report
/// their character value; Enter/Backspace/Tab/Escape report their control
/// values (13/8/9/27) so a text editor can treat them as characters.
///
/// Modifier keys (0x108-0x10B) and function keys (0x110+) are forwarded to a
/// bound compositor only: the kernel terminal consumes the modifiers, and a
/// function key with no compositor never reaches a task (issue #167). The
/// values are append-only and mirrored by `user::messenger::display::key`.
pub mod key {
    pub const ENTER: u32 = 13;
    pub const BACKSPACE: u32 = 8;
    pub const TAB: u32 = 9;
    pub const ESCAPE: u32 = 27;
    pub const SPACE: u32 = 32;
    pub const LEFT: u32 = 0x100;
    pub const RIGHT: u32 = 0x101;
    pub const UP: u32 = 0x102;
    pub const DOWN: u32 = 0x103;
    pub const PAGE_UP: u32 = 0x104;
    pub const PAGE_DOWN: u32 = 0x105;
    pub const HOME: u32 = 0x106;
    pub const END: u32 = 0x107;
    pub const SHIFT: u32 = 0x108;
    pub const CTRL: u32 = 0x109;
    pub const ALT: u32 = 0x10A;
    pub const SUPER: u32 = 0x10B;
    pub const F4: u32 = 0x113;
}

/// The bind op's output block: seven little-endian `u64` words.
pub const INFO_WORDS: usize = 7;
/// Framebuffer width in pixels.
pub const INFO_WIDTH: usize = 0;
/// Framebuffer height in pixels.
pub const INFO_HEIGHT: usize = 1;
/// Framebuffer stride in pixels (the screen buffer is tightly packed).
pub const INFO_STRIDE: usize = 2;
/// Framebuffer bytes per pixel (the screen buffer is always RGBA8).
pub const INFO_BPP: usize = 3;
/// Handle of the screen buffer, open in the caller's table.
pub const INFO_BUFFER: usize = 4;
/// Virtual address the screen buffer is mapped at.
pub const INFO_VA: usize = 5;
/// Screen buffer length in bytes (`width * height * 4`).
pub const INFO_SIZE: usize = 6;

/// Native display-syscall ops (syscall 12).
pub mod op {
    /// Claim the display for the calling task.
    pub const BIND: u64 = 0;
    /// Release the display; the kernel mux resumes painting.
    pub const UNBIND: u64 = 1;
    /// Drain queued input events into a user buffer.
    pub const INPUT_POLL: u64 = 2;
    /// Copy a damage rectangle from the screen buffer to the framebuffer.
    pub const PRESENT: u64 = 3;
    /// Create a shared buffer and map it; `[handle, va, size]` out.
    pub const CREATE_BUFFER: u64 = 4;
    /// Map an existing shared buffer; its address out.
    pub const MAP_BUFFER: u64 = 5;
    /// Close a shared-buffer handle (unmap + drop the reference).
    pub const CLOSE_BUFFER: u64 = 6;
}

/// Errno values, matching the Linux numbering the rest of the native ABI uses.
pub(crate) mod errno {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const EBADF: i64 = 9;
    pub const ENOMEM: i64 = 12;
    pub const EFAULT: i64 = 14;
    pub const EBUSY: i64 = 16;
    pub const EINVAL: i64 = 22;
}

/// Two's-complement `-errno` in the syscall return register.
pub(crate) fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Map a shared-buffer failure onto the display vocabulary.
pub(crate) fn shared_errno(error: shared::Error) -> u64 {
    use shared::Error::*;
    negative(match error {
        InvalidHandle | NotFound => errno::ENOENT,
        NoFreeHandle | RegistryFull | Quota | UserQuota | OutOfMemory => errno::ENOMEM,
        BadTask | WrongKind | BadFlags | BadSize | MissingRight | ShareOnly | BadDescriptor => {
            errno::EINVAL
        }
        ExecutableDenied | MapFailed | StaleSequence | TimedOut => errno::EINVAL,
    })
}

/// Framebuffer geometry copied from the boot info at startup.
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
static GRANT: Mutex<Option<Grant>> = Mutex::new(None);
/// Owner slot, readable without the grant lock so IRQ handlers can cheaply ask
/// "is a compositor bound?".
static OWNER: AtomicUsize = AtomicUsize::new(NO_OWNER);
static EVENTS: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());

/// Record the framebuffer geometry at boot (called from `kernel_main`, after
/// the console owns the framebuffer).
pub fn init(width: usize, height: usize, stride: usize, bytes_per_pixel: usize) {
    *SCREEN.lock() = Screen {
        width: width as u64,
        height: height as u64,
        stride: stride as u64,
        bytes_per_pixel: bytes_per_pixel as u64,
    };
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

/// The screen-buffer handle `slot` holds as the bound compositor, if any.
///
/// `close_buffer` refuses it: closing it would leave the grant pointing at a
/// freed mapping that `present` still blits from.
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
        op::PRESENT => present(a1),
        op::CREATE_BUFFER => buffers::create_buffer(a1, a2),
        op::MAP_BUFFER => buffers::map_buffer(a1, a2),
        op::CLOSE_BUFFER => buffers::close_buffer(a1),
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

    let screen = *SCREEN.lock();
    if screen.width == 0 || screen.height == 0 {
        return negative(errno::ENOENT);
    }
    let size = screen.width * screen.height * 4;
    let handle = match shared::create(size, shared::flags::READ | shared::flags::WRITE) {
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

/// syscall 12 op 3: blit a damage rectangle from the screen buffer to the real
/// framebuffer.
///
/// `packed` is `x | y << 16 | w << 32 | h << 48` with every field a `u16`; the
/// rectangle is clamped to the screen. Only the owner may present.
fn present(packed: u64) -> u64 {
    let owner = OWNER.load(Ordering::Relaxed);
    if owner == NO_OWNER || owner != task::current() {
        return negative(errno::EPERM);
    }
    let (x, y) = (
        (packed & 0xffff) as usize,
        ((packed >> 16) & 0xffff) as usize,
    );
    let (w, h) = (
        ((packed >> 32) & 0xffff) as usize,
        ((packed >> 48) & 0xffff) as usize,
    );
    let (va, size, width, height) = {
        let grant = GRANT.lock();
        let Some(grant) = grant.as_ref() else {
            return negative(errno::EPERM);
        };
        (
            grant.va,
            grant.size,
            grant.width as usize,
            grant.height as usize,
        )
    };
    if x >= width || y >= height || w == 0 || h == 0 {
        return 0;
    }
    let w = w.min(width - x);
    let h = h.min(height - y);
    // The owner's screen buffer is mapped at `va` in the active (caller's)
    // address space, but the owner can unmap it (a Linux compositor has
    // `munmap`), so re-validate before the kernel reads it.
    let Ok(pixels) = user_ptr::try_bytes(va, size as usize) else {
        return negative(errno::EFAULT);
    };
    console::with_framebuffer(|fb| fb.blit_rgba_region(pixels, width, height, x, y, x, y, w, h));
    0
}

/// Queue one event; drops the oldest when the queue is full.
///
/// Called from IRQ handlers (keyboard/mouse) and from [`bind`]; safe because
/// the queue lock is a leaf and interrupts are off in both paths.
pub fn push_event(event: Event) {
    if !bound() {
        return;
    }
    let mut events = EVENTS.lock();
    if events.len() >= MAX_EVENTS {
        events.pop_front();
    }
    events.push_back(event);
}

/// Queue a key press/release, translating the terminal key into a [`key`]
/// code. Called from the keyboard IRQ when a compositor is bound.
pub fn push_key(key: Key, down: bool) {
    let kind = if down { event::KEY_DOWN } else { event::KEY_UP };
    push_event(Event {
        kind,
        a: key_code(key) as i32,
        b: 0,
        reserved: 0,
    });
}

/// Queue a pointer move.
pub fn push_pointer_move(x: i32, y: i32) {
    push_event(Event {
        kind: event::POINTER_MOVE,
        a: x,
        b: y,
        reserved: 0,
    });
}

/// Queue a pointer button press/release.
pub fn push_pointer_button(button: u32, down: bool) {
    let kind = if down {
        event::POINTER_DOWN
    } else {
        event::POINTER_UP
    };
    push_event(Event {
        kind,
        a: button as i32,
        b: 0,
        reserved: 0,
    });
}

/// Translate a decoded terminal key into its display key code.
fn key_code(key: Key) -> u32 {
    match key {
        Key::Char(c) => c as u32,
        Key::Enter => key::ENTER,
        Key::Backspace => key::BACKSPACE,
        Key::Tab => key::TAB,
        Key::Escape => key::ESCAPE,
        Key::Space => key::SPACE,
        Key::Left => key::LEFT,
        Key::Right => key::RIGHT,
        Key::Up => key::UP,
        Key::Down => key::DOWN,
        Key::PageUp => key::PAGE_UP,
        Key::PageDown => key::PAGE_DOWN,
        Key::Home => key::HOME,
        Key::End => key::END,
        Key::Shift => key::SHIFT,
        Key::Ctrl => key::CTRL,
        Key::Alt => key::ALT,
        Key::Super => key::SUPER,
        Key::F4 => key::F4,
    }
}

/// Test-harness hook: forget any grant and drop queued events. The suite runs
/// without a scheduler, so there is nothing to close or wake.
#[cfg(lazyos_tests)]
pub fn reset() {
    *GRANT.lock() = None;
    OWNER.store(NO_OWNER, Ordering::Relaxed);
    EVENTS.lock().clear();
}
