//! The display grant (syscall 12, issue #113): binding the screen, the
//! owner's raw input queue, presenting, and the shared buffers every
//! compositor client draws into. Mirrors `kernel/src/display.rs`.

use crate::{nr, value, zero};

/// Display-syscall op codes.
pub mod display_op {
    /// Claim the display for this task (one owner at a time).
    pub const BIND: u64 = 0;
    /// Release the display; the kernel mux resumes painting.
    pub const UNBIND: u64 = 1;
    /// Drain queued input events into a byte buffer.
    pub const INPUT_POLL: u64 = 2;
    /// Copy a damage rectangle from the screen buffer to the framebuffer.
    pub const PRESENT: u64 = 3;
    /// Create a shared buffer and map it; `[handle, va, size]` out. Open to
    /// compositor *clients*, which never bind the display.
    pub const CREATE_BUFFER: u64 = 4;
    /// Map a shared buffer received from another task; its address out.
    pub const MAP_BUFFER: u64 = 5;
    /// Close a shared buffer handle (unmaps it, frees its quota charge).
    pub const CLOSE_BUFFER: u64 = 6;
    /// Declare the screen buffer's byte order ([`super::screen_layout`]).
    pub const SET_LAYOUT: u64 = 7;
    /// The byte order `present` copies without converting.
    pub const NATIVE_LAYOUT: u64 = 8;
}

/// Screen-buffer byte orders for [`display_set_layout`]; the fourth byte of a
/// pixel is ignored.
pub mod screen_layout {
    /// `R, G, B, A`: the layout after every bind.
    pub const RGBA: u64 = 0;
    /// `B, G, R, A`.
    pub const BGRA: u64 = 1;
}

/// Input event kinds, `kernel/src/display.rs::event`.
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
    /// Wheel rolled: `a` = notches, positive away from the user (scroll up).
    pub const POINTER_WHEEL: u32 = 5;
}

/// Pointer buttons, `kernel/src/display.rs::button`.
pub mod button {
    /// Left button.
    pub const LEFT: u32 = 1;
    /// Right button.
    pub const RIGHT: u32 = 2;
    /// Middle button.
    pub const MIDDLE: u32 = 3;
}

/// Keyboard codes, `kernel/src/display.rs::key`. Printable keys report their
/// character value; Enter/Backspace/Tab/Escape report their control values.
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

/// Bytes per record [`display_input_poll`] writes.
pub const EVENT_BYTES: usize = 16;

/// One input record of the display owner's queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayEvent {
    /// Event kind, one of [`event`].
    pub kind: u32,
    /// First payload word (position x, or key/button code).
    pub a: i32,
    /// Second payload word (position y).
    pub b: i32,
}

impl DisplayEvent {
    /// Decode record `index` of a poll buffer; `None` past the end.
    pub fn decode(bytes: &[u8], index: usize) -> Option<DisplayEvent> {
        let at = index.checked_mul(EVENT_BYTES)?;
        let record = bytes.get(at..at.checked_add(EVENT_BYTES)?)?;
        let word = |at: usize| {
            u32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]])
        };
        Some(DisplayEvent {
            kind: word(0),
            a: word(4) as i32,
            b: word(8) as i32,
        })
    }
}

/// The [`display_op::BIND`] output block, the kernel's seven words.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct DisplayInfo {
    /// Framebuffer width in pixels.
    pub width: u64,
    /// Framebuffer height in pixels.
    pub height: u64,
    /// Framebuffer stride in pixels (the screen buffer is tightly packed).
    pub stride: u64,
    /// Framebuffer bytes per pixel (the screen buffer is always RGBA8).
    pub bytes_per_pixel: u64,
    /// Screen-buffer handle, open in this task's table.
    pub buffer: u64,
    /// Screen-buffer mapping address.
    pub va: u64,
    /// Screen-buffer length in bytes (`width * height * 4`).
    pub size: u64,
}

/// A display op whose arguments are plain values.
fn plain(op: u64, a1: u64) -> i64 {
    // SAFETY: the callers pass no pointer (`UNBIND`, `PRESENT`, the buffer
    // handle ops, the layout ops).
    unsafe { crate::raw::syscall3(nr::DISPLAY, op, a1, 0) }
}

/// Bind the display to this task: the kernel mux stops painting and input
/// is queued for [`display_input_poll`]. Fills `info` with the screen
/// buffer's handle, address and geometry.
pub fn display_bind(info: &mut DisplayInfo) -> Result<(), i64> {
    // SAFETY: the kernel writes one `DisplayInfo` (seven words) to `info`,
    // exclusively borrowed for the call.
    let code = unsafe {
        crate::raw::syscall3(
            nr::DISPLAY,
            display_op::BIND,
            info as *mut DisplayInfo as u64,
            0,
        )
    };
    zero(code)
}

/// Release the display; the kernel multiplexer repaints from its own state.
pub fn display_unbind() -> Result<(), i64> {
    zero(plain(display_op::UNBIND, 0))
}

/// Drain queued input events into `buf` ([`EVENT_BYTES`] each), returning
/// the number of whole records written. Only the display owner may poll.
pub fn display_input_poll(buf: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`.
    let code = unsafe {
        crate::raw::syscall3(
            nr::DISPLAY,
            display_op::INPUT_POLL,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    value(code).map(|count| count as usize)
}

/// `(x, y, w, h)` packed into one word, 16 bits each; negative values are 0.
pub fn pack_rect(x: i32, y: i32, w: i32, h: i32) -> u64 {
    let field = |v: i32| (if v < 0 { 0 } else { v as u64 }) & 0xffff;
    field(x) | field(y) << 16 | field(w) << 32 | field(h) << 48
}

/// Present a damage rectangle from the bound screen buffer. Only the display
/// owner may present.
pub fn display_present(x: i32, y: i32, w: i32, h: i32) -> Result<(), i64> {
    zero(plain(display_op::PRESENT, pack_rect(x, y, w, h)))
}

/// Create a shared buffer of `size` bytes, mapped into this task; returns
/// `(handle, address, size)`. Pass the handle to the compositor to attach it.
pub fn display_create_buffer(size: u64) -> Result<(u64, u64, u64), i64> {
    let mut words = [0u64; 3];
    // SAFETY: the kernel writes three words to `words`.
    let code = unsafe {
        crate::raw::syscall3(
            nr::DISPLAY,
            display_op::CREATE_BUFFER,
            size,
            words.as_mut_ptr() as u64,
        )
    };
    zero(code).map(|()| (words[0], words[1], words[2]))
}

/// Map a shared-buffer handle received from another task; its address here.
pub fn display_map_buffer(handle: u64) -> Result<u64, i64> {
    let mut va = 0u64;
    // SAFETY: the kernel writes one word to `va`.
    let code = unsafe {
        crate::raw::syscall3(
            nr::DISPLAY,
            display_op::MAP_BUFFER,
            handle,
            &mut va as *mut u64 as u64,
        )
    };
    zero(code).map(|()| va)
}

/// Close a shared buffer: unmaps it and releases the per-process buffer
/// quota. The compositor's own reference keeps an attached surface's pixels
/// alive until it detaches.
pub fn display_close_buffer(handle: u64) -> Result<(), i64> {
    zero(plain(display_op::CLOSE_BUFFER, handle))
}

/// Declare the byte order the owner draws its screen buffer in (one of
/// [`screen_layout`]); `present` converts from it. Every bind starts at RGBA.
pub fn display_set_layout(layout: u64) -> Result<(), i64> {
    zero(plain(display_op::SET_LAYOUT, layout))
}

/// The screen-buffer byte order `present` copies as is (a plain row copy),
/// or `Err(-ENOENT)` when the framebuffer has none (every present converts).
pub fn display_native_layout() -> Result<u64, i64> {
    value(plain(display_op::NATIVE_LAYOUT, 0))
}

/// The bound display: unbound when dropped.
#[derive(Debug)]
pub struct DisplayGrant {
    info: DisplayInfo,
}

impl DisplayGrant {
    /// Bind the display ([`display_bind`]).
    pub fn bind() -> Result<DisplayGrant, i64> {
        let mut info = DisplayInfo::default();
        display_bind(&mut info)?;
        Ok(DisplayGrant { info })
    }

    /// The screen buffer's geometry and mapping.
    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }
}

impl Drop for DisplayGrant {
    fn drop(&mut self) {
        // The kernel also unbinds a task that exits; a failure here leaves
        // nothing to undo.
        let _ = display_unbind();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_packs_sixteen_bits_per_field_and_clamps_negatives() {
        assert_eq!(pack_rect(1, 2, 3, 4), 1 | 2 << 16 | 3 << 32 | 4 << 48);
        assert_eq!(pack_rect(-5, 0x1_0001, 0, -1), 1 << 16);
    }

    #[test]
    fn events_decode_by_index_and_stop_at_the_end() {
        let mut bytes = [0u8; EVENT_BYTES * 2];
        bytes[16..20].copy_from_slice(&event::KEY_DOWN.to_le_bytes());
        bytes[20..24].copy_from_slice(&(key::F1 as i32).to_le_bytes());
        bytes[24..28].copy_from_slice(&(-7i32).to_le_bytes());
        let second = DisplayEvent::decode(&bytes, 1).unwrap();
        assert_eq!(
            (second.kind, second.a, second.b),
            (event::KEY_DOWN, key::F1 as i32, -7)
        );
        assert!(DisplayEvent::decode(&bytes, 2).is_none());
        assert!(DisplayEvent::decode(&bytes[..20], 1).is_none());
    }
}
