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
//!   rax = 6  rdi -> "PATH [args...]" (NUL-terminated)   -> pid | -1
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
//!   op 2 (spawn): rsi -> "PATH [args...]" (NUL),       rdx -> Cred block
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
//!
//! # The device syscall (issue #240)
//!
//! Syscall 23 lets an unprivileged driver (holding `CAP_DEV_CLAIM`) claim a
//! device and reach its BARs, ports, PCI config and interrupt through a
//! `Device` handle. Arguments travel in `rdi` (op), `rsi`, `rdx`, `r10` and
//! `r8`; the op table and every check live in [`crate::dev::syscall`]. The
//! routing table for all syscalls is in [`gate`].

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::{PhysAddr, VirtAddr};

use crate::ipc::credentials::{self, Cred, TransitionError};
use crate::mem::vma::{Kind, Prot};
use crate::quota::{self, Resource};
use crate::task::{self, wait::CHILD_EXIT, WakeReason};
use crate::user_ptr;
use crate::{fs, input::keyboard, mem};

#[allow(unused_imports)] // part of the module ABI; referenced by tests and userspace docs
pub use creds::cred_op;
use creds::{sys_creds, sys_quota, sys_tasks};

mod argstore;
mod credio;
mod creds;
pub(crate) mod elfhdr;
mod exec_perm;
pub mod fsops;
pub(crate) mod gate;
pub mod image;
pub mod inetsys;
pub mod killsys;
pub mod layout;
pub mod linux;
pub mod loader;
pub mod power;
pub mod randsys;
mod spawn;
pub mod spawnv;
pub mod wallsys;

use credio::{read_cred, write_cred};
#[cfg(lazyos_tests)]
pub use gate::dispatch_for_test;
pub use gate::syscall_gate;
pub use loader::{load_segments, Loaded};

/// Lowest address of a native heap: `sbrk` starts here, or at the page after
/// the image when the image reaches past it ([`layout::heap_start`]).
pub const USER_HEAP_BASE: u64 = 0x60_0000;
/// Top of the native user stack (grows down): the shared stack top of
/// [`layout`]. The heap grows up to [`NATIVE_HEAP_LIMIT`], far below it.
pub const USER_STACK_TOP: u64 = layout::STACK_TOP;
/// Native user stack size, mapped eagerly (native programs are LazyOS's own
/// and keep their frames small).
pub const USER_STACK_SIZE: u64 = 0x2_0000;
/// Ceiling of a native `sbrk` heap: the start of the region the kernel lays
/// out itself (pages are demand-zero, so an unused ceiling costs nothing; the
/// per-uid quota bounds what is reserved).
pub const NATIVE_HEAP_LIMIT: u64 = layout::MMAP_BASE;

/// Record the `argv` of the native task in `slot`, `argv[0]` included, with an
/// empty environment: the per-task block `spawnv` fills, for the spawns that
/// do not come through it (the kernel's boot spawns and the Linux `execve` of
/// a native program, `process::linux::native`). Each item is one argument as
/// given, never split; syscall 9 hands the block to the program.
pub(crate) fn set_task_argv<A: AsRef<[u8]>>(slot: usize, argv: &[A]) {
    argstore::set(slot, argstore::block(argv), alloc::vec::Vec::new());
}

/// Forget the argument blocks of the task in `slot`; the task table calls
/// this when it frees the slot.
pub(crate) fn forget_task_args(slot: usize) {
    argstore::forget(slot);
}

/// Test-harness view of how many task slots hold argument blocks.
#[cfg(lazyos_tests)]
pub fn task_args_live_for_test() -> usize {
    argstore::live_count()
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
    // A program a shell `execve`d inherits the shell's descriptors, so its
    // output follows a redirection or pipe on fd 1 (issue #315); every other
    // native task has the terminal there and takes the path below.
    if let Some(written) = linux::write_redirected(ptr, len) {
        return written;
    }
    let Ok(bytes) = user_ptr::try_bytes(ptr, len as usize) else {
        return u64::MAX;
    };
    task::write_output(bytes);
    crate::serial::write_bytes(bytes);
    len
}

/// syscall 2: block until a key is routed to this task, then return its code.
fn sys_read_char() -> u64 {
    // A program a shell `execve`d reads the shell's stdin when it is a pipe or
    // file, not the kernel key queue (issue #315).
    if let Some(byte) = linux::read_redirected() {
        return byte;
    }
    // Park on the terminal queue between checks (`on_key` notifies it), so an
    // idle reader such as `logind` is Blocked rather than Runnable: a napping
    // poll loop stayed runnable and took every other tick from the desktop's
    // apps (issue #373). Syscalls run with interrupts off, so a key cannot
    // land between the check and the park, and `take_key` (the task table)
    // never runs preemptibly (issue #382).
    let key = loop {
        if let Some(key) = task::take_key() {
            break key;
        }
        if task::wait_terminal() == WakeReason::Interrupted {
            return 0;
        }
    };
    match key {
        keyboard::Key::Char(c) => c as u64,
        keyboard::Key::Enter => b'\n' as u64,
        keyboard::Key::Space => b' ' as u64,
        keyboard::Key::Backspace => 8,
        keyboard::Key::Tab => b'\t' as u64,
        keyboard::Key::Escape => 27,
        _ => 0,
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
    if new_break > NATIVE_HEAP_LIMIT {
        return u64::MAX;
    }
    let table = mem::kernel_table();
    // Never grow over another mapping (`vma::insert` would turn it into heap).
    if new_break > current && !mem::vma::find_range(table, current, new_break).is_empty() {
        return u64::MAX;
    }
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
        // A killed caller returns (as a timeout it never sees) so the gate
        // can end it; a wait with no deadline would otherwise park again.
        if timeout == WakeReason::Interrupted && task::signal::killed(me) {
            return u64::MAX;
        }
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

/// Where a loaded native program starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeStart {
    pub entry: u64,
    /// The initial `sbrk` break.
    pub heap: u64,
}

/// Load a static ELF64 image (streamed from `image`) and map the native user
/// stack.
pub fn load_image<I: image::Image + ?Sized>(
    table: PhysAddr,
    image: &I,
) -> Result<NativeStart, &'static str> {
    let loaded = load_segments(table, image, &layout::IMAGE_RESERVED)?;
    map_range_kind(
        table,
        USER_STACK_TOP - USER_STACK_SIZE,
        USER_STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    )?;
    Ok(NativeStart {
        entry: loaded.entry,
        heap: layout::heap_start(loaded.end, USER_HEAP_BASE),
    })
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
        let phys = mem::alloc_zeroed_frame().ok_or(loader::OUT_OF_MEMORY)?;
        if !mem::map_page_in(table, VirtAddr::new(va), phys, mem::prot_flags(prot)) {
            return Err(loader::MAP_PAGE_FAILED);
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
