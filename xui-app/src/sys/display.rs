//! The display-grant syscall client: op codes, input event kinds, the bind
//! output block and the `bind`/`unbind`/`poll`/`present`/buffer calls.

use super::{native, SYS_DISPLAY};

/// Display-syscall op codes, mirroring `kernel/src/display.rs`.
pub mod op {
    /// Claim the display for this task (one owner at a time).
    pub const BIND: u64 = 0;
    /// Release the display.
    pub const UNBIND: u64 = 1;
    /// Drain queued input events into a byte buffer.
    pub const INPUT_POLL: u64 = 2;
    /// Copy a damage rectangle from the screen buffer to the framebuffer.
    pub const PRESENT: u64 = 3;
    /// Create a shared buffer and map it; `[handle, va, size]` out. Also
    /// available to compositor *clients*, which never bind the display.
    pub const CREATE_BUFFER: u64 = 4;
    /// Map a shared buffer received from another task; its address out.
    pub const MAP_BUFFER: u64 = 5;
    /// Close a shared buffer handle (unmaps it, frees its quota charge).
    pub const CLOSE_BUFFER: u64 = 6;
}

/// Input event kinds, mirroring `kernel/src/display.rs::event`.
pub mod event {
    /// Pointer moved: `a` = x, `b` = y.
    pub const POINTER_MOVE: u32 = 0;
    /// Pointer button pressed: `a` = button.
    pub const POINTER_DOWN: u32 = 1;
    /// Pointer button released: `a` = button.
    pub const POINTER_UP: u32 = 2;
    /// Key pressed: `a` = key code.
    pub const KEY_DOWN: u32 = 3;
    /// Key released: `a` = key code.
    pub const KEY_UP: u32 = 4;
}

/// Pointer buttons, mirroring `kernel/src/display.rs::button`.
pub mod button {
    /// Left button.
    pub const LEFT: u32 = 1;
    /// Right button.
    pub const RIGHT: u32 = 2;
    /// Middle button.
    pub const MIDDLE: u32 = 3;
}
/// Keyboard codes, mirroring `kernel/src/display.rs::key`. Printable keys
/// report their character value; Enter/Backspace/Tab/Escape report their
/// control values.
pub mod key {
    /// Enter.
    pub const ENTER: u32 = 13;
    /// Backspace.
    pub const BACKSPACE: u32 = 8;
    /// Tab.
    pub const TAB: u32 = 9;
    /// Escape.
    pub const ESCAPE: u32 = 27;
    /// Space.
    pub const SPACE: u32 = 32;
    /// Left arrow.
    pub const LEFT: u32 = 0x100;
    /// Right arrow.
    pub const RIGHT: u32 = 0x101;
    /// Up arrow.
    pub const UP: u32 = 0x102;
    /// Down arrow.
    pub const DOWN: u32 = 0x103;
    /// Page up.
    pub const PAGE_UP: u32 = 0x104;
    /// Page down.
    pub const PAGE_DOWN: u32 = 0x105;
    /// Home.
    pub const HOME: u32 = 0x106;
    /// End.
    pub const END: u32 = 0x107;
    /// Left/right Shift (one code; the kernel tracks both).
    pub const SHIFT: u32 = 0x108;
    /// Left/right Ctrl.
    pub const CTRL: u32 = 0x109;
    /// Left/right Alt.
    pub const ALT: u32 = 0x10A;
    /// Left/right Super (the Windows/Cmd key).
    pub const SUPER: u32 = 0x10B;
    /// Delete (forward delete).
    pub const DELETE: u32 = 0x10C;
    /// Insert.
    pub const INSERT: u32 = 0x10D;
    /// Function key `F1`; `Fn` is `F1 + n - 1`, so `F4` is `0x113`.
    pub const F1: u32 = 0x110;
    /// The last function key.
    pub const F12: u32 = 0x11B;

    /// Modifier bits the compositor ORs into the key of every `KeyDown`/`KeyUp`
    /// it forwards to a client (bits 24..=27). The kernel's own records never
    /// carry them; a client in owner mode tracks the modifier keys instead.
    /// See `docs/architecture/display.md`, "Key codes clients receive".
    pub const MOD_SHIFT: u32 = 1 << 24;
    /// Ctrl held.
    pub const MOD_CTRL: u32 = 1 << 25;
    /// Alt held.
    pub const MOD_ALT: u32 = 1 << 26;
    /// Super (Windows/Cmd) held.
    pub const MOD_SUPER: u32 = 1 << 27;
    /// Mask selecting the key code from a forwarded key.
    pub const CODE_MASK: u32 = 0x00FF_FFFF;
}

/// Bytes per encoded input event.
pub const EVENT_BYTES: usize = 16;

/// One raw input record, decoded from `input_poll`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RawEvent {
    /// Event kind, one of [`event`].
    pub kind: u32,
    /// First payload word (position x or key/button code).
    pub a: i32,
    /// Second payload word (position y).
    pub b: i32,
}

/// The [`op::BIND`] output block, the kernel's seven little-endian `u64` words.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayInfo {
    /// Framebuffer width in pixels.
    pub width: u64,
    /// Framebuffer height in pixels.
    pub height: u64,
    /// Framebuffer stride in pixels.
    pub stride: u64,
    /// Framebuffer bytes per pixel (always 4).
    pub bytes_per_pixel: u64,
    /// Screen-buffer handle in this task's table.
    pub buffer: u64,
    /// Screen-buffer mapping address.
    pub va: u64,
    /// Screen-buffer length in bytes.
    pub size: u64,
}
/// One `display` syscall; returns 0 or a negative errno.
fn display_syscall(op: u64, a1: u64, a2: u64) -> i64 {
    native(SYS_DISPLAY, op, a1, a2)
}
/// Claim the display; fills `info` with the screen buffer's handle, address and
/// geometry. The kernel mux stops painting while this task owns the display.
pub fn display_bind(info: &mut DisplayInfo) -> Result<(), i64> {
    let code = display_syscall(op::BIND, info as *mut DisplayInfo as u64, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Release the display; the kernel multiplexer repaints from its own state.
pub fn display_unbind() -> Result<(), i64> {
    let code = display_syscall(op::UNBIND, 0, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Drain queued input events into `buf` (16 bytes each), returning the number
/// of whole records written. Only the display owner may poll.
pub fn display_input_poll(buf: &mut [u8]) -> Result<usize, i64> {
    let code = display_syscall(op::INPUT_POLL, buf.as_mut_ptr() as u64, buf.len() as u64);
    if code >= 0 {
        Ok(code as usize)
    } else {
        Err(code)
    }
}

/// Present a damage rectangle from the screen buffer to the framebuffer.
pub fn display_present(x: i32, y: i32, w: i32, h: i32) -> Result<(), i64> {
    let packed = (x.max(0) as u64 & 0xffff)
        | ((y.max(0) as u64 & 0xffff) << 16)
        | ((w.max(0) as u64 & 0xffff) << 32)
        | ((h.max(0) as u64 & 0xffff) << 48);
    let code = display_syscall(op::PRESENT, packed, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Create a shared buffer of `size` bytes, mapped into this task; returns
/// `(handle, va, size)`. This is the compositor-client path for a surface's
/// backing store: binding the display is not required.
pub fn display_create_buffer(size: u64) -> Result<(u64, u64, u64), i64> {
    let mut words = [0u64; 3];
    let code = display_syscall(op::CREATE_BUFFER, size, words.as_mut_ptr() as u64);
    if code == 0 {
        Ok((words[0], words[1], words[2]))
    } else {
        Err(code)
    }
}

/// Close a buffer from [`display_create_buffer`]: unmaps it and releases the
/// per-process buffer quota. A surface it was attached to keeps its pixels
/// through the compositor's own reference until the surface is destroyed.
pub fn display_close_buffer(handle: u64) -> Result<(), i64> {
    let code = display_syscall(op::CLOSE_BUFFER, handle, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}
/// Decode the `index`-th record of a poll buffer.
pub fn decode_event(bytes: &[u8], index: usize) -> Option<RawEvent> {
    let base = index * EVENT_BYTES;
    if base + EVENT_BYTES > bytes.len() {
        return None;
    }
    let word = |at: usize| -> u32 {
        u32::from_le_bytes([
            bytes[base + at],
            bytes[base + at + 1],
            bytes[base + at + 2],
            bytes[base + at + 3],
        ])
    };
    Some(RawEvent {
        kind: word(0),
        a: word(4) as i32,
        b: word(8) as i32,
    })
}
