//! Thin wrappers around the LazyOS `int 0x80` syscalls.
//!
//! Register convention: `rax` = syscall number, args in `rdi`, `rsi`, `rdx`,
//! result in `rax`.
//!
//! The kernel's `int 0x80` stub saves and restores
//! `rdi`/`rsi`/`rdx`/`r8`/`r9`/`r10` around the Rust dispatcher, and `rax`
//! carries the result. `rcx` and `r11` are *not* preserved, so every wrapper
//! declares `clobber_abi("sysv64")`; otherwise the compiler may keep a live
//! value in them across the gate (issue #91 hit exactly that in `bootstrap()`).

use core::arch::asm;

/// `exit(code)` — terminate the program.
pub const SYS_EXIT: u64 = 0;
/// `write(ptr, len)` — write bytes to the console.
pub const SYS_WRITE: u64 = 1;
/// `read_char()` — block until a key is pressed, return its code.
pub const SYS_READ_CHAR: u64 = 2;
/// `read_file(name, buf, len)` — read a file into a buffer.
pub const SYS_READ_FILE: u64 = 3;
/// `sbrk(incr)` — grow the heap, returning the previous break.
pub const SYS_SBRK: u64 = 4;
/// `messenger(op, args, result)` — the native Messenger surface (issue #69).
pub const SYS_MESSENGER: u64 = 5;
/// `spawn(cmdline)` — start an ELF as a child of the caller (issue #93).
pub const SYS_SPAWN: u64 = 6;
/// `wait(deadline)` — reap a child exit, packing `(pid << 32) | status`.
pub const SYS_WAIT: u64 = 7;
/// `clock()` — the PIT tick counter (100 Hz), absolute deadlines.
pub const SYS_CLOCK: u64 = 8;
/// `args(buf, len)` — copy this service's manifest argument string.
pub const SYS_ARGS: u64 = 9;
/// `creds(op, a1, a2)` — the audited credential gate (issue #101).
pub const SYS_CREDS: u64 = 10;
/// `display(op, a1, a2)` — the display device grant (issue #113).
pub const SYS_DISPLAY: u64 = 12;
/// `tasks(buf)` — scheduler task-list introspection (MCP debug bridge Phase 2).
pub const SYS_TASKS: u64 = 13;

/// Credential-gate op codes, mirroring the kernel's `process::cred_op`.
pub mod cred_op {
    /// Stamp a task with a credential block.
    pub const SET: u64 = 0;
    /// Read a task's credential block.
    pub const GET: u64 = 1;
    /// Spawn an ELF with a credential block, stamped before it can run.
    pub const SPAWN: u64 = 2;
}

/// Value returned by the service syscalls on failure/timeout.
pub const SERVICE_ERROR: u64 = u64::MAX;

/// A task's kernel-stamped identity (issue #101), the userspace mirror of
/// `kernel/src/ipc/credentials.rs::Cred`.
///
/// Userspace can never choose this freely: [`cred_set`]/[`spawn_as`] ask the
/// kernel to validate and audit the request, and the kernel refuses a stamp
/// that would widen the caller's privilege. Login reads the user database,
/// builds one of these, and hands it to `spawn_as`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Cred {
    /// User id; `0` is the system/root user.
    pub uid: u32,
    /// Primary group id.
    pub gid: u32,
    /// Capability bits (`CAP_*`).
    pub caps: u32,
    /// Policy label id.
    pub label_id: u32,
    /// Session id; `0` before login.
    pub session: u64,
}

impl Cred {
    /// Build a credential.
    pub const fn new(uid: u32, gid: u32, caps: u32, label_id: u32, session: u64) -> Cred {
        Cred {
            uid,
            gid,
            caps,
            label_id,
            session,
        }
    }

    /// The 40-byte wire block the native gate reads and writes.
    pub const fn to_words(self) -> [u64; 5] {
        [
            self.uid as u64,
            self.gid as u64,
            self.caps as u64,
            self.label_id as u64,
            self.session,
        ]
    }

    /// Decode the 40-byte wire block.
    pub const fn from_words(words: [u64; 5]) -> Cred {
        Cred {
            uid: words[0] as u32,
            gid: words[1] as u32,
            caps: words[2] as u32,
            label_id: words[3] as u32,
            session: words[4],
        }
    }
}

/// Invoke the native credential gate. Returns `0`/pid, or a negative errno.
fn creds_syscall(op: u64, a1: u64, a2: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 10; the kernel validates the credential
    // request and refuses anything the caller may not do.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_CREDS,
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

/// Ask the kernel to stamp `target` (`None` = this task) with `cred`.
///
/// Only a service holding the set-credentials capability may do this, and the
/// kernel refuses any request that would widen the caller's own privilege; the
/// error is the negative errno (`-EPERM`, `-EACCES`, `-ESRCH`, ...).
pub fn cred_set(target: Option<u64>, cred: &Cred) -> Result<(), i64> {
    let words = cred.to_words();
    let code = creds_syscall(
        cred_op::SET,
        target.unwrap_or(u64::MAX),
        words.as_ptr() as u64,
    );
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// Read `target`'s kernel-stamped credentials (`None` = this task) into `out`.
pub fn cred_get(target: Option<u64>, out: &mut Cred) -> Result<(), i64> {
    let mut words = [0u64; 5];
    let code = creds_syscall(
        cred_op::GET,
        target.unwrap_or(u64::MAX),
        words.as_mut_ptr() as u64,
    );
    if code == 0 {
        *out = Cred::from_words(words);
        Ok(())
    } else {
        Err(code)
    }
}

/// Spawn the program named by a **NUL-terminated** command line as a child of
/// the calling task, stamped with `cred` before it can execute one
/// instruction. Returns the child's pid, or `None` when the kernel refused the
/// request or the program could not be started.
///
/// This is the login path: `logind` authenticates a user and starts the user's
/// shell already owning that user's identity, with no window in which the
/// child could run as root.
pub fn spawn_as(cmdline_z: &[u8], cred: &Cred) -> Option<u64> {
    let words = cred.to_words();
    let code = creds_syscall(
        cred_op::SPAWN,
        cmdline_z.as_ptr() as u64,
        words.as_ptr() as u64,
    );
    (code >= 0).then_some(code as u64)
}

/// Write raw bytes to the console.
pub fn write(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Safety: `int 0x80` with syscall 1 and a valid buffer.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_WRITE,
            in("rdi") bytes.as_ptr() as u64,
            in("rsi") bytes.len() as u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
}

/// Write a string to the console.
pub fn write_str(text: &str) {
    write(text.as_bytes());
}

/// Block until a key is pressed; returns its character code.
pub fn read_char() -> u64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 2; result in rax.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_READ_CHAR,
            lateout("rax") code,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code
}

/// Read a file named by a **NUL-terminated** byte slice into `buf`.
/// Returns the number of bytes read, or `None` if the file was not found.
pub fn read_file(name_z: &[u8], buf: &mut [u8]) -> Option<usize> {
    let count: u64;
    // Safety: `int 0x80` with syscall 3; pointers are valid for their lengths.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_READ_FILE,
            in("rdi") name_z.as_ptr() as u64,
            in("rsi") buf.as_mut_ptr() as u64,
            in("rdx") buf.len() as u64,
            lateout("rax") count,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    if count == u64::MAX {
        None
    } else {
        Some(count as usize)
    }
}

/// Grow the heap by `increment` bytes; returns the previous break, or
/// `u64::MAX` on failure. Calling with 0 reports the current break.
pub fn sbrk(increment: u64) -> u64 {
    let previous: u64;
    // Safety: `int 0x80` with syscall 4.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_SBRK,
            in("rdi") increment,
            lateout("rax") previous,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    previous
}

/// Copy a [`crate::messenger::TaskSnapshot`] scheduler snapshot into `buf`,
/// a writable buffer of at least `crate::messenger::TaskSnapshot::SIZE` bytes.
/// Returns 0 on success or a negative errno (mirrors `sys_quota`'s shape).
pub fn tasks(buf: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 13 and a valid writable buffer pointer.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_TASKS,
            in("rdi") buf,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// Invoke the native Messenger syscall: `op` selects the operation, `args`
/// and `result` are user addresses of the fixed-size blocks defined in
/// [`crate::messenger`]. Returns 0 on success or a negative errno.
pub fn messenger(op: u64, args: u64, result: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 5; the kernel validates both pointers
    // against this task's address space before touching them.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_MESSENGER,
            in("rdi") op,
            in("rsi") args,
            in("rdx") result,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// Terminate the program; does not return.
pub fn exit(code: u32) -> ! {
    // Safety: `int 0x80` with syscall 0; does not return.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_EXIT,
            in("rdi") code as u64,
            options(noreturn, nostack),
            clobber_abi("sysv64"),
        );
    }
}

/// Start the program named by a **NUL-terminated** command line
/// (`"PATH.ELF [args...]"`) as a child of the calling task. Returns the child's
/// pid (its task slot), or `None` when the file is missing or no resource is
/// free. The kernel remembers the argument string for [`service_args`].
pub fn spawn(cmdline_z: &[u8]) -> Option<u64> {
    let pid: u64;
    // Safety: `int 0x80` with syscall 6 and a valid NUL-terminated buffer.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_SPAWN,
            in("rdi") cmdline_z.as_ptr() as u64,
            lateout("rax") pid,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    (pid != SERVICE_ERROR).then_some(pid)
}

/// Wait for a child exit and reap it, up to the absolute PIT `deadline`
/// (`0` waits forever). Returns `Some((pid, status))`, or `None` on timeout.
pub fn wait(deadline: u64) -> Option<(u64, u64)> {
    let packed: u64;
    // Safety: `int 0x80` with syscall 7; no pointers cross the gate.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_WAIT,
            in("rdi") deadline,
            lateout("rax") packed,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    (packed != SERVICE_ERROR).then_some((packed >> 32, packed & 0xffff_ffff))
}

/// The PIT tick counter (100 Hz), the supervisor's clock. Deadlines passed to
/// [`wait`] and to the Messenger API are absolute values of this clock.
pub fn clock() -> u64 {
    let ticks: u64;
    // Safety: `int 0x80` with syscall 8; no arguments.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_CLOCK,
            lateout("rax") ticks,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    ticks
}

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

/// Copy this service's manifest argument string into `buf`; returns its full
/// length. A zero-length `buf` reports the length without copying.
pub fn service_args(buf: &mut [u8]) -> usize {
    let length: u64;
    // Safety: `int 0x80` with syscall 9; the buffer is valid for its length.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_ARGS,
            in("rdi") buf.as_mut_ptr() as u64,
            in("rsi") buf.len() as u64,
            lateout("rax") length,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    length as usize
}
