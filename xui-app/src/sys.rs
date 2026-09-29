//! Raw native-syscall shim for the display grant (12), the Messenger fabric
//! (5), the PIT clock (8) and the system-stats snapshot (14).
//!
//! Besides the raw `int 0x80` helpers, this module carries the small
//! libmessenger-based plumbing the display protocol client ([`crate::display`])
//! needs: a direct registry `resolve`, a synchronous `call`, an endpoint
//! `recv`, and `create_pair`.
//!
//! Register convention (see `user/src/sys.rs` in the LazyOS tree): `rax` is the
//! syscall number, arguments in `rdi`/`rsi`/`rdx`, the result in `rax`. `int
//! 0x80` is the native gate and is dispatched by task, not by binary kind, so a
//! static musl program reaches the same code a native `user` program does.
//! `rcx`/`r11` are not preserved by the gate.

use core::arch::asm;

use libmessenger::{Encoder, Header, Parcel, VERSION};

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

/// An absolute PIT tick in the past: `recv` treats it as a non-blocking poll
/// (mirrors `user::messenger::EXPIRED_DEADLINE`).
pub const EXPIRED_DEADLINE: u64 = 1;

/// Linux errno values used by the parcel helpers (positive forms).
pub mod errno {
    /// No such file or directory / service.
    pub const ENOENT: i64 = 2;
    /// The receive buffer is too small.
    pub const E2BIG: i64 = 7;
    /// Invalid argument.
    pub const EINVAL: i64 = 22;
    /// The peer endpoint is gone.
    pub const EPIPE: i64 = 32;
    /// A deadline fired.
    pub const ETIMEDOUT: i64 = 110;
}

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

/// Messenger op codes, mirroring `user/src/messenger/::op`.
pub mod msg_op {
    /// Call a method and block until the reply arrives.
    pub const CALL: u64 = 1;
    /// Receive one queued message.
    pub const RECV: u64 = 4;
    /// Close an endpoint handle.
    pub const CLOSE_ENDPOINT: u64 = 6;
    /// Create a fresh channel pair; both handles open in this task.
    pub const CREATE_PAIR: u64 = 7;
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

/// Run one Messenger syscall carrying `MsgArgs`/`MsgResult` blocks; `Ok` when
/// the syscall returned 0, `Err(negative errno)` otherwise.
fn messenger_syscall(op: u64, args: &MsgArgs, result: &mut MsgResult) -> Result<(), i64> {
    let code = messenger(
        op,
        args as *const MsgArgs as u64,
        result as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(code)
    } else {
        Ok(())
    }
}

/// Create a fresh Messenger channel pair; both handles open in this task.
///
/// The compositor protocol moves one end to `xuid` inside `CreateSurface` and
/// receives input events on the other.
pub fn msg_create_pair() -> Result<(u64, u64), i64> {
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::CREATE_PAIR, &MsgArgs::default(), &mut result)?;
    Ok((result.value, result.aux))
}

/// Resolve `name` into this task's handle table through the kernel registry.
///
/// The request is a `libmessenger` parcel with a single `NAME` string field;
/// the kernel opens the service's published endpoint straight into the
/// caller's table and returns its handle in `MsgResult::value`.
pub fn msg_resolve(name: &str) -> Result<u64, i64> {
    /// Registry interface id: the first eight bytes of `os.lazy.…`.
    const REGISTRY_INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");
    /// Registry method `resolve`.
    const REGISTRY_RESOLVE: u32 = 2;
    /// Registry TLV field id for a name.
    const REGISTRY_FIELD_NAME: u16 = 1;

    let mut body = Encoder::new();
    body.string(REGISTRY_FIELD_NAME, name)
        .map_err(|_| -errno::EINVAL)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: REGISTRY_INTERFACE,
            method: REGISTRY_RESOLVE,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -errno::EINVAL)?;
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::RESOLVE, &args, &mut result)?;
    Ok(result.value)
}

/// One synchronous `call`: send `parcel` on `handle`, wait for the reply into
/// `buf` (bounded by `deadline`, an absolute PIT tick; `0` waits forever), and
/// decode it.
pub fn msg_call(
    handle: u64,
    parcel: &Parcel,
    buf: &mut [u8],
    deadline: u64,
) -> Result<Parcel, i64> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -errno::EINVAL)?;
    let args = MsgArgs {
        handle,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        deadline,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::CALL, &args, &mut result)?;
    let len = result.bytes as usize;
    if len > buf.len() {
        return Err(-errno::E2BIG);
    }
    Parcel::decode(&buf[..len]).map_err(|_| -errno::EINVAL)
}

/// Receive one queued message into `buf`; the full [`MsgResult`] carries the
/// reply length (`bytes`) and transaction id (`value`). `deadline` is an
/// absolute PIT tick, or [`EXPIRED_DEADLINE`] for a non-blocking poll.
pub fn msg_recv(handle: u64, buf: &mut [u8], deadline: u64) -> Result<MsgResult, i64> {
    let args = MsgArgs {
        handle,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        deadline,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    messenger_syscall(msg_op::RECV, &args, &mut result)?;
    if result.bytes as usize > buf.len() {
        return Err(-errno::E2BIG);
    }
    Ok(result)
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
