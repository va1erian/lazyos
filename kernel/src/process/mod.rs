//! Ring-3 execution: the `int 0x80` syscall gate and a static ELF64 loader.
//!
//! The loader maps a program into a given address space ([`load_image`]); the
//! scheduler (`crate::task`) then runs it in ring 3. Syscalls reach the kernel
//! through the gate installed at vector `0x80`.
//!
//! # Service syscalls (issue #93)
//!
//! The userspace `init` supervisor needs three things the demo surface did not
//! provide: to start a program as *its* child (`spawn = 6`), to wait for a
//! child exit so a crash can be restarted (`wait = 7`), and absolute timer
//! ticks to schedule backoff and polls (`clock = 8`). A fourth call
//! (`args = 9`) hands a service the argument string its manifest entry
//! declared, because native programs have no `argv` stack yet:
//!
//! ```text
//!   rax = 6  rdi -> "PATH.ELF [args...]" (NUL-terminated)   -> pid | -1
//!   rax = 7  rdi = absolute PIT deadline (0 = forever)       -> pid<<32 | status, or -1
//!   rax = 8                                                  -> PIT ticks
//!   rax = 9  rdi -> buffer, rsi = capacity                   -> argument length
//! ```
//!
//! `spawn` reads the ELF from the FAT image, calls [`task::spawn_child`] and
//! remembers the argument string by slot; the kernel intern table behind task
//! names holds one leaked string per distinct service name, so a restart loop
//! cannot grow it.
//!
//! # The credential gate (issue #101)
//!
//! Accounts and login need one controlled way to stamp a task's
//! `uid/gid/caps/label/session`. Syscall 10 is that gate: a single op code with
//! a 40-byte credential block shared with `user::sys`, guarded by
//! [`crate::ipc::credentials`]:
//!
//! ```text
//!   rax = 10  rdi = op
//!   op 0 (set):   rsi = target pid (u64::MAX = caller), rdx -> Cred block
//!   op 1 (get):   rsi = target pid (u64::MAX = caller), rdx <- Cred block
//!   op 2 (spawn): rsi -> "PATH.ELF [args...]" (NUL),       rdx -> Cred block
//! ```
//!
//! Every request is validated by [`credentials::transition`] (only an actor
//! holding `CAP_SETUID` may stamp, never toward more privilege) and audited.
//! `spawn` stamps the child inside the same syscall, before the interrupt gate
//! can schedule it, so a login shell never runs even briefly with the default
//! root identity. Returns `0` (`set`/`get`), the new pid (`spawn`), or
//! `-errno`: `-EPERM` without the capability, `-EACCES` for a widening request,
//! `-ESRCH` for an unknown target, `-EFAULT` for an invalid block, `-EINVAL`
//! for an unknown op.
//!
//! # The quota gate (issue #103)
//!
//! Syscall 11 is the read side of the per-uid quota table
//! ([`crate::quota`]): it copies the caller's usage and limits into a
//! `2 * Resource::COUNT`-word block so a service can explain a refusal to its
//! user. Setting limits is kernel policy, not a syscall.
//!
//! ```text
//!   rax = 11  rdi -> [usage, limit] pairs in Resource order  -> 0 | -EFAULT
//! ```
//!
//! # The display device grant (issue #113)
//!
//! Syscall 12 hands the framebuffer and the PS/2 input stream to a userspace
//! compositor (`docs/platform-plan.md` S4.4). The op codes, the bind output
//! block and the input event records live in [`crate::display`]; the gate here
//! is one arm because the kernel-side state is one small module.
//!
//! ```text
//!   rax = 12  rdi = op
//!   op 0 (bind):          rsi -> [width, height, stride, bpp, buffer, va, size]
//!   op 1 (unbind):        -
//!   op 2 (input_poll):    rsi -> events, rdx = capacity  -> count
//!   op 3 (present):       rsi = packed damage
//!   op 4 (create_buffer): rsi = size, rdx -> [handle, va, size]
//!   op 5 (map_buffer):    rsi = handle, rdx -> va
//! ```
//!
//! # The system-stats snapshot (issue #144)
//!
//! Syscall 14 is a read-only monitor surface: `sysmond` serves it over
//! Messenger and `top` renders it. The fixed layout, version, buffer contract
//! and the deliberate "readable by every task, no addresses or credentials"
//! permission choice live in [`crate::sysinfo`].
//!
//! ```text
//!   rax = 14  rdi = op
//!   op 0 (snapshot): rsi -> buffer, rdx = capacity in bytes  -> size | -errno
//!   op 1 (size):                                             -> size
//! ```

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::global_asm;
use spin::Mutex;
use x86_64::structures::idt::HandlerFunc;
use x86_64::{PhysAddr, VirtAddr};

use crate::ipc::credentials::{self, Cred, TransitionError};
use crate::mem::vma::{Kind, Prot};
use crate::quota::{self, Resource};
use crate::task::{self, wait::CHILD_EXIT, WakeReason};
use crate::user_ptr;
use crate::{fs, input::keyboard, mem};

#[allow(unused_imports)] // part of the module ABI; referenced by tests and userspace docs
pub use creds::cred_op;
use creds::{sys_creds, sys_quota, sys_tasks, syscall_error, EFAULT};
use spawn::{sys_spawn, SERVICE_ARGS};

mod credio;
mod creds;
pub mod fsops;
pub mod linux;
pub mod loader;
pub mod power;
mod spawn;
pub mod spawn_line;

use credio::{read_cred, write_cred};
pub use loader::load_segments;

/// Base of the user heap (grows up toward the stack).
pub const USER_HEAP_BASE: u64 = 0x60_0000;
/// Top of the user stack (grows down).
pub const USER_STACK_TOP: u64 = 0x80_0000;
/// User stack size.
pub const USER_STACK_SIZE: u64 = 0x2_0000;

/// Saved general-purpose registers, laid out to match the syscall stub's pushes.
#[repr(C)]
struct Regs {
    rax: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
}

// Syscall entry stub: save argument registers, dispatch, restore, iretq.
global_asm!(
    r#"
    .global syscall_isr
    syscall_isr:
        push rdi
        push rsi
        push rdx
        push r8
        push r9
        push r10
        push rax
        mov rdi, rsp
        call syscall_dispatch
        pop rax
        pop r10
        pop r9
        pop r8
        pop rdx
        pop rsi
        pop rdi
        iretq
    "#
);

extern "C" {
    fn syscall_isr();
}

/// The handler to install at vector `0x80` (DPL 3).
pub fn syscall_gate() -> HandlerFunc {
    // Safety: `syscall_isr` is a naked ISR with a compatible (no ABI) signature.
    unsafe { core::mem::transmute::<*const (), HandlerFunc>(syscall_isr as *const ()) }
}

#[no_mangle]
extern "C" fn syscall_dispatch(regs: *mut Regs) {
    // Safety: the stub passes a valid pointer to saved registers.
    let regs = unsafe { &mut *regs };
    // Reclaim slots the scheduler flagged (issue #133): on a syscall entry the
    // current task holds no heap lock, so dropping dead tasks is safe.
    task::reclaim_pending();
    if regs.rax == 0 {
        exit(regs.rdi as u32);
    }
    regs.rax = match regs.rax {
        1 => sys_write(regs.rdi, regs.rsi),
        2 => sys_read_char(),
        3 => sys_read_file(regs.rdi, regs.rsi, regs.rdx),
        4 => sys_sbrk(regs.rdi),
        // 5: the native Messenger surface (issue #69): `rdi` is the op code,
        // `rsi` points at a `MsgArgs` block and `rdx` at a `MsgResult` block.
        5 => crate::ipc::syscalls::dispatch(regs.rdi, regs.rsi, regs.rdx),
        // 6..9: the service supervision surface (issue #93).
        6 => sys_spawn(regs.rdi),
        7 => sys_wait(regs.rdi),
        8 => sys_clock(),
        9 => sys_args(regs.rdi, regs.rsi),
        // 10: the credential gate (issue #101), see the module docs.
        10 => sys_creds(regs.rdi, regs.rsi, regs.rdx),
        // 11: per-uid quota introspection (issue #103), read-only.
        11 => sys_quota(regs.rdi),
        // 12: the display device grant (issue #113), see the module docs.
        12 => crate::display::dispatch(regs.rdi, regs.rsi, regs.rdx),
        // 13: scheduler task-list introspection (MCP debug bridge Phase 2),
        // read-only.
        13 => sys_tasks(regs.rdi),
        // 14: the system-stats snapshot (issue #144), read-only and available
        // to every task; see `crate::sysinfo` and the module docs.
        14 => crate::sysinfo::dispatch(regs.rdi, regs.rsi, regs.rdx),
        // 15..22: the shell's filesystem calls, `power`, and `fsync` (issue
        // #6, #260).
        15..=22 => fsops::dispatch(regs.rax, regs.rdi, regs.rsi, regs.rdx),
        _ => u64::MAX,
    };
}

/// Test-harness entry into the native syscall surface (issue #62 pattern):
/// drive one syscall exactly as the `int 0x80` gate would, without the ring
/// transition. Compiled only for the in-kernel suite.
#[cfg(lazyos_tests)]
pub fn dispatch_for_test(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    match nr {
        1 => sys_write(a1, a2),
        3 => sys_read_file(a1, a2, a3),
        4 => sys_sbrk(a1),
        5 => crate::ipc::syscalls::dispatch(a1, a2, a3),
        6 => sys_spawn(a1),
        7 => sys_wait(a1),
        8 => sys_clock(),
        9 => sys_args(a1, a2),
        10 => sys_creds(a1, a2, a3),
        11 => sys_quota(a1),
        12 => crate::display::dispatch(a1, a2, a3),
        13 => sys_tasks(a1),
        14 => crate::sysinfo::dispatch(a1, a2, a3),
        15..=22 => fsops::dispatch(nr, a1, a2, a3),
        _ => u64::MAX,
    }
}

/// Test-harness view of [`intern_service_name`], so the suite can prove the
/// intern table is bounded.
#[cfg(lazyos_tests)]
pub fn intern_service_name_for_test(name: &str) -> &'static str {
    spawn::intern_service_name(name)
}

/// syscall 1: write bytes to the task's terminal (and the serial log).
///
/// The buffer is validated against the caller's page tables; a bad range
/// returns the `u64::MAX` failure code instead of touching kernel memory.
fn sys_write(ptr: u64, len: u64) -> u64 {
    let Ok(bytes) = user_ptr::try_bytes(ptr, len as usize) else {
        return u64::MAX;
    };
    task::write_output(bytes);
    crate::serial::write_bytes(bytes);
    len
}

/// syscall 2: block until a key is routed to this task, then return its code.
fn sys_read_char() -> u64 {
    loop {
        if let Some(key) = task::take_key() {
            return match key {
                keyboard::Key::Char(c) => c as u64,
                keyboard::Key::Enter => b'\n' as u64,
                keyboard::Key::Space => b' ' as u64,
                keyboard::Key::Backspace => 8,
                keyboard::Key::Tab => b'\t' as u64,
                keyboard::Key::Escape => 27,
                _ => 0,
            };
        }
        // Interrupts are disabled inside the gate; enable them so the timer can
        // preempt us (letting other tasks run) and the keyboard can deliver keys.
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// Longest NUL-terminated string a native syscall reads from user memory.
const USER_CSTR_MAX: usize = 4096;

/// Read a NUL-terminated string (at most [`USER_CSTR_MAX`] bytes) from
/// validated user memory. Invalid UTF-8 reads as the empty string, as it
/// always has; an unmapped/kernel address or an unterminated string is a Fault.
pub(crate) fn user_cstr(ptr: u64) -> Result<String, user_ptr::Fault> {
    let bytes = user_ptr::try_cstr(ptr, USER_CSTR_MAX).map_err(|_| user_ptr::Fault)?;
    Ok(String::from_utf8(bytes).unwrap_or_default())
}

/// syscall 3: read a file into a user buffer. Returns the count, or `u64::MAX`.
fn sys_read_file(name_ptr: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    let Ok(name) = user_cstr(name_ptr) else {
        return u64::MAX;
    };
    match fs::read(&name) {
        Some(bytes) => {
            let count = bytes.len().min(buf_len as usize);
            if user_ptr::try_copy_to(buf_ptr, &bytes[..count]).is_err() {
                return u64::MAX;
            }
            count as u64
        }
        None => u64::MAX,
    }
}

/// syscall 4: grow this task's heap; returns the previous break or `u64::MAX`.
///
/// The new range is recorded as a `Heap` VMA and populated on first touch
/// (demand-zero), so a large `sbrk` costs no frames until the program uses
/// them. Shrinking releases the pages under the new break.
fn sys_sbrk(increment: u64) -> u64 {
    let current = task::heap_break();
    if increment == 0 {
        return current;
    }
    let page = 4096u64;
    let Some(target) = current.checked_add(increment) else {
        return u64::MAX;
    };
    let new_break = match target.checked_add(page - 1) {
        Some(value) => value & !(page - 1),
        None => return u64::MAX,
    };
    if new_break > USER_STACK_TOP - USER_STACK_SIZE {
        return u64::MAX;
    }
    let table = mem::kernel_table();
    if new_break > current {
        // Per-uid user-memory quota (issue #103): charge the growth before the
        // VMA exists; a refusal returns the unchanged break like any other
        // size failure.
        let delta = new_break - current;
        if quota::charge_for_slot(task::current(), Resource::UserMemory, delta).is_err() {
            return u64::MAX;
        }
        mem::vma::insert(
            table,
            current,
            new_break,
            Prot::READ | Prot::WRITE,
            Kind::Heap,
        );
    } else if new_break < current {
        let delta = current - new_break;
        mem::vma::remove(table, new_break, current);
        mem::unmap_range(table, new_break, current);
        quota::release_for_slot(task::current(), Resource::UserMemory, delta);
    }
    task::set_heap_break(new_break);
    current
}

/// syscall 0: terminate the current task.
///
/// The exit status is recorded on the task so the supervisor's `wait` (syscall
/// 7) can see *why* a service died and apply its restart policy.
fn exit(code: u32) -> ! {
    serial_println!("user: task exited with status {code}");
    task::finish_current(code as u64);
    // Wait for the scheduler to switch to another task.
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// syscall 7: wait for a child exit and reap it.
///
/// `deadline` is an absolute PIT tick, `0` waits forever. Returns the packed
/// `(pid << 32) | status`, or `u64::MAX` on timeout. The syscall parks on the
/// child-exit queue with interrupts disabled (the `int 0x80` gate), so no exit
/// can slip between the reap check and the park.
fn sys_wait(deadline: u64) -> u64 {
    let me = task::current();
    loop {
        if let Some((slot, status)) = task::reap_child() {
            return pack_exit(slot, status);
        }
        let timeout = CHILD_EXIT.wait(me, (deadline != 0).then_some(deadline));
        if timeout == WakeReason::TimedOut {
            // A child may have exited on the very tick the deadline passed.
            return match task::reap_child() {
                Some((slot, status)) => pack_exit(slot, status),
                None => u64::MAX,
            };
        }
    }
}

/// Pack a reaped child's slot and exit status into one register.
fn pack_exit(slot: usize, status: u64) -> u64 {
    (slot as u64) << 32 | (status & 0xffff_ffff)
}

/// syscall 8: the PIT tick counter (100 Hz), the supervisor's clock.
fn sys_clock() -> u64 {
    task::ticks()
}

/// syscall 9: copy this task's service argument string into `buf`.
///
/// Returns the full argument length; at most `buf_len` bytes are copied, so a
/// caller can size the buffer from a first zero-capacity call.
fn sys_args(buf_ptr: u64, buf_len: u64) -> u64 {
    let args = SERVICE_ARGS.lock()[task::current()]
        .clone()
        .unwrap_or_default();
    let count = args.len().min(buf_len as usize);
    if count > 0 && user_ptr::try_copy_to(buf_ptr, &args[..count]).is_err() {
        return syscall_error(EFAULT);
    }
    args.len() as u64
}

/// Load a static ELF64 image and map the native user stack.
pub fn load_image(table: PhysAddr, elf_bytes: &[u8]) -> Result<u64, &'static str> {
    let entry = load_segments(table, elf_bytes, &[(USER_HEAP_BASE, USER_STACK_TOP)])?;
    map_range_kind(
        table,
        USER_STACK_TOP - USER_STACK_SIZE,
        USER_STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    )?;
    Ok(entry)
}

/// Map `[start, end)` as zeroed anonymous user pages into `table` (eager), for
/// callers that must have the pages present immediately. Linux `mmap`/`brk`
/// prefer the lazy VMA path; the kernel test suite uses this to build scratch
/// address spaces.
#[allow(dead_code)]
pub fn map_range(table: PhysAddr, start: u64, end: u64) -> Result<Vec<(u64, u64)>, &'static str> {
    map_range_kind(table, start, end, Prot::READ | Prot::WRITE, Kind::Anon)
}

/// [`map_range`] with an explicit protection and VMA kind.
pub fn map_range_kind(
    table: PhysAddr,
    start: u64,
    end: u64,
    prot: Prot,
    kind: Kind,
) -> Result<Vec<(u64, u64)>, &'static str> {
    let mut pages = Vec::new();
    let mut va = start & !0xFFF;
    while va < end {
        let phys = mem::alloc_zeroed_frame().ok_or("out of memory")?;
        if !mem::map_page_in(table, VirtAddr::new(va), phys, mem::prot_flags(prot)) {
            return Err("failed to map user page");
        }
        pages.push((va, phys.as_u64()));
        va += 4096;
    }
    mem::vma::insert(table, start, end, prot, kind);
    Ok(pages)
}

/// Physical frame backing a page recorded by [`map_range`].
pub fn page_phys(pages: &[(u64, u64)], va: u64) -> Option<u64> {
    phys_for(pages, va)
}

fn phys_for(mappings: &[(u64, u64)], va: u64) -> Option<u64> {
    let page = va & !0xFFF;
    mappings
        .iter()
        .find(|(page_vaddr, _)| *page_vaddr == page)
        .map(|(_, phys)| *phys)
}
