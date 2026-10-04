//! The display syscall ABI: event records, `INFO_*` block layout, op numbers and errnos.

use crate::ipc::shared;

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
    /// Wheel rolled: `a` = notches, positive away from the user (scroll up),
    /// negative toward the user (scroll down). Only wheel mice produce it.
    pub const POINTER_WHEEL: u32 = 5;
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
    pub const DELETE: u32 = 0x10C;
    pub const INSERT: u32 = 0x10D;
    /// `F1`; function key `n` (1..=12) is `F1 + n - 1` (so `F4` is 0x113).
    pub const F1: u32 = 0x110;
    /// Documented ABI values; only the test suite names them.
    #[allow(dead_code)]
    pub const F4: u32 = 0x113;
    #[allow(dead_code)]
    pub const F12: u32 = 0x11B;
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
    /// Declare the screen buffer's byte order (one of [`super::layout`]).
    pub const SET_LAYOUT: u64 = 7;
    /// The byte order `present` copies without conversion, or `-ENOENT`.
    pub const NATIVE_LAYOUT: u64 = 8;
}

/// Screen-buffer byte orders for ops 7 and 8 (the fourth byte is ignored).
pub mod layout {
    /// `R, G, B, A`: the default after every bind.
    pub const RGBA: u64 = 0;
    /// `B, G, R, A`.
    pub const BGRA: u64 = 1;
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
