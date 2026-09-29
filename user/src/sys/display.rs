//! The display device grant (issue #113), wrapping syscall 12.

use core::arch::asm;

use super::SYS_DISPLAY;

// ---------------------------------------------------------------------------
// Display device grant (issue #113)
// ---------------------------------------------------------------------------

/// Display-syscall op codes, mirroring `kernel/src/display.rs`.
pub mod display_op {
    /// Claim the display for this task (one compositor at a time).
    pub const BIND: u64 = 0;
    /// Release the display; the kernel mux resumes painting.
    pub const UNBIND: u64 = 1;
    /// Drain queued input events into a byte buffer.
    pub const INPUT_POLL: u64 = 2;
    /// Copy a damage rectangle from the screen buffer to the framebuffer.
    pub const PRESENT: u64 = 3;
    /// Create a shared buffer and map it.
    pub const CREATE_BUFFER: u64 = 4;
    /// Map an existing shared buffer.
    pub const MAP_BUFFER: u64 = 5;
    /// Close a shared buffer handle (unmaps it).
    pub const CLOSE_BUFFER: u64 = 6;
}

/// The [`display_op::BIND`] output block, mirroring the kernel's seven words.
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

/// One `display` syscall. Returns 0 or a negative errno.
fn display_syscall(op: u64, a1: u64, a2: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 12; the kernel validates pointers with
    // the native syscall convention (trusted user buffers).
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_DISPLAY,
            in("rdi") op,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// Bind the display to this task: the kernel mux stops painting and input
/// events are queued for [`display_input_poll`]. Fills `info` with the screen
/// buffer's handle, address and geometry.
pub fn display_bind(info: &mut DisplayInfo) -> Result<(), i64> {
    let code = display_syscall(display_op::BIND, info as *mut DisplayInfo as u64, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Release the display; the kernel multiplexer repaints from its own state.
pub fn display_unbind() -> Result<(), i64> {
    let code = display_syscall(display_op::UNBIND, 0, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Drain queued input events into `buf` (16 bytes each), returning the number
/// of whole events written. Only the bound compositor may poll.
pub fn display_input_poll(buf: &mut [u8]) -> Result<usize, i64> {
    let code = display_syscall(
        display_op::INPUT_POLL,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    );
    if code >= 0 {
        Ok(code as usize)
    } else {
        Err(code)
    }
}

/// Present a damage rectangle from the bound screen buffer. Only the bound
/// compositor may present.
pub fn display_present(x: i32, y: i32, w: i32, h: i32) -> Result<(), i64> {
    let packed = (x.max(0) as u64 & 0xffff)
        | ((y.max(0) as u64 & 0xffff) << 16)
        | ((w.max(0) as u64 & 0xffff) << 32)
        | ((h.max(0) as u64 & 0xffff) << 48);
    let code = display_syscall(display_op::PRESENT, packed, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Create a shared buffer of `size` bytes, mapped into this task. Returns
/// `(handle, address)`; pass the handle to the compositor with
/// `messenger::display::Client::attach_buffer`.
pub fn display_create_buffer(size: u64) -> Result<(u64, u64), i64> {
    let mut words = [0u64; 3];
    let code = display_syscall(display_op::CREATE_BUFFER, size, words.as_mut_ptr() as u64);
    if code == 0 {
        Ok((words[0], words[1]))
    } else {
        Err(code)
    }
}

/// Close a shared buffer created with [`display_create_buffer`]: unmaps it
/// and releases the per-process buffer quota. The compositor's own reference
/// keeps an attached surface's pixels alive until it detaches.
pub fn display_close_buffer(handle: u64) -> Result<(), i64> {
    let code = display_syscall(display_op::CLOSE_BUFFER, handle, 0);
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Map a shared-buffer handle received from another task and return its
/// address in this task's address space.
pub fn display_map_buffer(handle: u64) -> Result<u64, i64> {
    let mut va = 0u64;
    let code = display_syscall(display_op::MAP_BUFFER, handle, &mut va as *mut u64 as u64);
    if code == 0 {
        Ok(va)
    } else {
        Err(code)
    }
}
