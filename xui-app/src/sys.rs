//! Raw native-syscall shim for the display grant (12), the Messenger fabric
//! (5), the PIT clock (8) and the system-stats snapshot (14).
//!
//! Register convention (see `user/src/sys.rs` in the LazyOS tree): `rax` is the
//! syscall number, arguments in `rdi`/`rsi`/`rdx`, the result in `rax`. `int
//! 0x80` is the native gate and is dispatched by task, not by binary kind, so a
//! static musl program reaches the same code a native `user` program does.
//! `rcx`/`r11` are not preserved by the gate.

use core::arch::asm;

/// `display(op, a1, a2)` — the display device grant.
pub const SYS_DISPLAY: u64 = 12;
/// `messenger(op, args, result)` — the native Messenger fabric.
pub const SYS_MESSENGER: u64 = 5;
/// `clock()` — the PIT tick counter (100 Hz), absolute deadlines.
pub const SYS_CLOCK: u64 = 8;
/// `system_stats(op, a1, a2)` — the read-only system monitor (issue #144).
pub const SYS_SYSTEM_STATS: u64 = 14;
/// `nanosleep(req, rem)` — the Linux ABI's relative sleep.
pub const SYS_NANOSLEEP: u64 = 35;

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

/// Messenger op codes, mirroring `user/src/messenger.rs::op`.
pub mod msg_op {
    /// Call a method and block until the reply arrives.
    pub const CALL: u64 = 1;
    /// Close an endpoint handle.
    pub const CLOSE_ENDPOINT: u64 = 6;
    /// Read the versioned fabric snapshot (`FabricStats`).
    pub const STATS: u64 = 8;
    /// Resolve a service name to a new handle.
    pub const RESOLVE: u64 = 14;
    /// Snapshot the name table into the caller's buffer.
    pub const LIST: u64 = 16;
}

/// `MsgArgs::txn_id` marker for registry ops: act on the calling task.
pub const REGISTRY_TARGET_SELF: u64 = u64::MAX;

/// The Messenger syscall request block; mirrors the kernel's `MsgArgs`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MsgArgs {
    /// Endpoint handle: call, begin, send, recv, cancel, close, stats.
    pub handle: u64,
    /// Transaction id, or the registry target task.
    pub txn_id: u64,
    /// Request parcel bytes.
    pub parcel_ptr: u64,
    /// Request parcel length in bytes.
    pub parcel_len: u64,
    /// Reply or receive buffer.
    pub buf_ptr: u64,
    /// Capacity of `buf_ptr` in bytes.
    pub buf_cap: u64,
    /// Absolute PIT deadline; 0 waits forever.
    pub deadline: u64,
    /// Reserved; must be zero.
    pub flags: u64,
}

/// The Messenger syscall response block; mirrors the kernel's `MsgResult`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MsgResult {
    /// 0 on success, or a negative errno.
    pub status: i64,
    /// New handle (resolve), transaction id (begin/recv).
    pub value: u64,
    /// Second handle (create_pair), sender task slot (recv).
    pub aux: u64,
    /// Bytes written to `buf_ptr`.
    pub bytes: u64,
    /// Delivered transfers for `recv`; zero otherwise.
    pub reserved: [u64; 4],
}

/// System-stats op codes, mirroring `kernel/src/sysinfo.rs::op`.
pub mod system_stats_op {
    /// Write the snapshot into the caller's buffer.
    pub const SNAPSHOT: u64 = 0;
    /// Report the snapshot's size in bytes.
    pub const SIZE: u64 = 1;
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

/// One native syscall through the `int 0x80` gate; the raw result register.
fn native(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with the native syscall convention. The kernel runs
    // the gate on this task's page table and validates every pointer argument
    // against the caller's address space.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// One `display` syscall; returns 0 or a negative errno.
fn display_syscall(op: u64, a1: u64, a2: u64) -> i64 {
    native(SYS_DISPLAY, op, a1, a2)
}

/// The PIT tick counter (100 Hz). `0` before the first tick.
pub fn clock_ticks() -> u64 {
    native(SYS_CLOCK, 0, 0, 0) as u64
}

/// One `system_stats` syscall; `SIZE`, 0 or a negative errno.
pub fn system_stats(op: u64, a1: u64, a2: u64) -> i64 {
    native(SYS_SYSTEM_STATS, op, a1, a2)
}

/// One `messenger` syscall; 0 or a negative errno.
pub fn messenger(op: u64, args: u64, result: u64) -> i64 {
    native(SYS_MESSENGER, op, args, result)
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

/// Sleep for `millis` milliseconds.
///
/// `nanosleep` is a *Linux* syscall, so it goes through the `syscall` gate
/// (like the rest of `std`); the native `int 0x80` dispatcher used by the
/// display ops above has no sleep. A relative timespec is used rather than
/// `std::thread::sleep`, whose absolute `clock_nanosleep` deadline LazyOS
/// currently treats as a duration.
pub fn sleep_millis(millis: u64) {
    let request: [i64; 2] = [(millis / 1000) as i64, ((millis % 1000) * 1_000_000) as i64];
    // Safety: `syscall` with the Linux nanosleep number (35) and a two-`i64`
    // `timespec` the kernel reads from this task's address space.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_NANOSLEEP,
            in("rdi") request.as_ptr() as u64,
            in("rsi") 0u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
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
