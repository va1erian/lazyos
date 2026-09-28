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
//! Syscall 13 is a read-only monitor surface: `sysmond` serves it over
//! Messenger and `top` renders it. The fixed layout, version, buffer contract
//! and the deliberate "readable by every task, no addresses or credentials"
//! permission choice live in [`crate::sysinfo`].
//!
//! ```text
//!   rax = 13  rdi = op
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
use xmas_elf::program::{SegmentData, Type as ProgramType};
use xmas_elf::ElfFile;

use crate::ipc::credentials::{self, Cred, TransitionError};
use crate::mem::vma::{Kind, Prot};
use crate::quota::{self, Resource};
use crate::task::{self, wait::CHILD_EXIT, WakeReason};
use crate::user_ptr;
use crate::{fs, input::keyboard, mem};

pub mod linux;

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
        _ => u64::MAX,
    };
}

/// Test-harness entry into the native syscall surface (issue #62 pattern):
/// drive one syscall exactly as the `int 0x80` gate would, without the ring
/// transition. Compiled only for the in-kernel suite.
#[cfg(laZYOS_TESTS)]
pub fn dispatch_for_test(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    match nr {
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
        _ => u64::MAX,
    }
}

/// syscall 1: write bytes to the task's terminal (and the serial log).
fn sys_write(ptr: u64, len: u64) -> u64 {
    // Safety: syscalls only pass pointers into the (mapped) user address
    // space (the syscall ABI's contract).
    let bytes = unsafe { user_ptr::bytes(ptr, len as usize) };
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

/// Read a NUL-terminated string from user memory.
fn user_cstr(ptr: u64) -> &'static str {
    let mut len = 0usize;
    // Safety: the caller must pass a valid, NUL-terminated user pointer (the
    // syscall ABI's contract).
    unsafe {
        while len < 4096 && user_ptr::read_at::<u8>(ptr, len) != 0 {
            len += 1;
        }
        let bytes = user_ptr::bytes(ptr, len);
        core::str::from_utf8(bytes).unwrap_or("")
    }
}

/// syscall 3: read a file into a user buffer. Returns the count, or `u64::MAX`.
fn sys_read_file(name_ptr: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    let name = user_cstr(name_ptr);
    match fs::read(name) {
        Some(bytes) => {
            let count = bytes.len().min(buf_len as usize);
            // Safety: the destination is a valid user buffer of `buf_len`
            // bytes (the syscall ABI's contract).
            unsafe { user_ptr::copy_to(buf_ptr, &bytes[..count]) };
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
    let new_break = (target + page - 1) & !(page - 1);
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

/// Service argument strings, keyed by task slot (issue #93).
///
/// Native programs receive no `argv`/`argc` stack, so `spawn` stores the
/// manifest argument string here and syscall 9 (or `sys::service_args`) copies
/// it out. The entry is overwritten on the slot's next state-changing spawn and
/// only read by that slot, so a re-used slot cannot observe stale arguments of
/// a *different* program (a plain kernel `spawn` clears the slot).
static SERVICE_ARGS: Mutex<[Option<Vec<u8>>; task::MAX_TASKS]> =
    Mutex::new([const { None }; task::MAX_TASKS]);

/// Intern a userspace-provided service name into a `&'static str` for
/// [`task::spawn_child`].
///
/// `Task::name` is `&'static str`, but the name comes from the supervisor's
/// manifest at runtime. Leaking each *distinct* name once (bounded by the
/// manifest, not by restart count) is the smallest way to satisfy that type
/// without adding an allocation policy to the task table.
fn intern_service_name(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock();
    if let Some(known) = names.iter().find(|known| **known == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(String::from(name).into_boxed_str());
    names.push(leaked);
    leaked
}

/// syscall 6: start `"PATH.ELF [args...]"` as a child of the calling task.
///
/// The command line is NUL-terminated. The first whitespace-separated token is
/// the FAT file name, the remainder is stored for syscall 9. Returns the new
/// task's pid (its slot), or `u64::MAX` when the file is missing, the ELF is
/// invalid, or no slot/frame is free.
fn sys_spawn(cmdline_ptr: u64) -> u64 {
    let code = spawn_program(cmdline_ptr, None);
    if code < 0 {
        u64::MAX
    } else {
        code as u64
    }
}

/// The shared body of syscalls 6 and 10 (`spawn` and the credentialed spawn).
///
/// `cred` is `Some` only on the credential-gate path, where the caller has
/// already validated the request with [`credentials::check`]. The slot's
/// credentials are reset first, so a re-used slot can never inherit a dead
/// task's identity, then the requested credential is stamped while interrupts
/// are off in the `int 0x80` gate -- the child cannot run with the default
/// root identity even for one instruction. Negative return values are errno
/// codes; a positive value is the new child's pid.
fn spawn_program(cmdline_ptr: u64, cred: Option<Cred>) -> i64 {
    let line = user_cstr(cmdline_ptr).trim();
    if line.is_empty() {
        return -EINVAL;
    }
    let (path, args) = match line.split_once(char::is_whitespace) {
        Some((path, args)) => (path, args.trim()),
        None => (line, ""),
    };
    let Some(elf) = fs::read(path) else {
        return -ENOENT;
    };
    let name = intern_service_name(path);
    let slot = match task::spawn_child(name, &elf) {
        Ok(slot) => slot,
        Err(_) => return -ENOMEM,
    };
    credentials::reset_for_task(slot);
    if let Some(cred) = cred {
        // `check` ran before the spawn, so this cannot fail; if it ever did,
        // the child would keep the reset root default and the gate would still
        // audit the refusal, which is the loudest signal available here.
        let _ = credentials::transition(task::current(), slot, cred);
    }
    SERVICE_ARGS.lock()[slot] = Some(args.as_bytes().to_vec());
    slot as i64
}

/// Error values the credential gate returns; the same x86_64 Linux numbering
/// the Messenger syscall uses, so userspace handling is uniform.
const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const ESRCH: i64 = 3;
const ENOMEM: i64 = 12;
const EACCES: i64 = 13;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;

/// The credential-gate op codes (syscall 10), mirrored by `user::sys`.
pub mod cred_op {
    /// Stamp a task with a credential block.
    pub const SET: u64 = 0;
    /// Read a task's credential block.
    pub const GET: u64 = 1;
    /// Spawn an ELF with a credential block, stamped before it can run.
    pub const SPAWN: u64 = 2;
}

/// Two's-complement `-errno` in the syscall return register.
fn syscall_error(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Map a transition refusal to its errno value.
fn transition_error(error: TransitionError) -> u64 {
    syscall_error(match error {
        TransitionError::NotPrivileged => EPERM,
        TransitionError::Widening => EACCES,
        TransitionError::BadTarget => ESRCH,
    })
}

/// The task slot named by a `set`/`get` target: the caller for `u64::MAX`,
/// otherwise the pid.
fn cred_target(pid: u64) -> usize {
    if pid == u64::MAX {
        task::current()
    } else {
        usize::try_from(pid).unwrap_or(usize::MAX)
    }
}

/// Read a 40-byte credential block from user memory.
///
/// The `int 0x80` stub runs on the caller's page table, so the block is
/// directly readable; a malformed pointer faults inside the kernel exactly as
/// it would for the older native syscalls (checked copies are the COW/MM
/// follow-up noted in `docs/security-model.md` section 7).
fn read_cred(ptr: u64) -> Option<Cred> {
    if ptr == 0 {
        return None;
    }
    let mut words = [0u64; 5];
    for (index, word) in words.iter_mut().enumerate() {
        // Safety: the caller must pass a mapped, writable user buffer eight
        // bytes per word; the native syscall ABI trusts user buffers today.
        *word = unsafe { user_ptr::read_at::<u64>(ptr, index) };
    }
    Some(Cred::from_words(words))
}

/// Write a 40-byte credential block into user memory; `false` on a null
/// pointer.
fn write_cred(ptr: u64, cred: Cred) -> bool {
    if ptr == 0 {
        return false;
    }
    for (index, word) in cred.to_words().iter().enumerate() {
        // Safety: as in [`read_cred`]; the address is the caller's buffer.
        unsafe { user_ptr::write_at::<u64>(ptr, index, *word) };
    }
    true
}

/// syscall 10: the audited credential gate (issue #101).
///
/// Every path funnels through [`credentials::transition`]/[`credentials::read`],
/// so the capability check, the no-widening rule, and the audit record live in
/// one place. See the module docs for the register ABI.
fn sys_creds(op: u64, a1: u64, a2: u64) -> u64 {
    match op {
        cred_op::SET => {
            let Some(cred) = read_cred(a2) else {
                return syscall_error(EFAULT);
            };
            match credentials::transition(task::current(), cred_target(a1), cred) {
                Ok(_) => 0,
                Err(error) => transition_error(error),
            }
        }
        cred_op::GET => match credentials::read(task::current(), cred_target(a1)) {
            Ok(cred) => {
                if write_cred(a2, cred) {
                    0
                } else {
                    syscall_error(EFAULT)
                }
            }
            Err(error) => transition_error(error),
        },
        cred_op::SPAWN => {
            let Some(cred) = read_cred(a2) else {
                return syscall_error(EFAULT);
            };
            // Validate before a task exists, then let `spawn_program` apply the
            // same request.
            if let Err(error) = credentials::check(task::current(), cred) {
                return transition_error(error);
            }
            let code = spawn_program(a1, Some(cred));
            if code < 0 {
                syscall_error(-code)
            } else {
                code as u64
            }
        }
        _ => syscall_error(EINVAL),
    }
}

/// syscall 11: copy the calling user's quota usage and limits (issue #103).
///
/// `buf` points at [`quota::STATS_WORDS`] `u64`s: for resource `i`, word `2*i`
/// is the live usage and word `2*i + 1` the limit, in [`Resource::ALL`] order.
/// A null buffer is `-EFAULT`; limits themselves are kernel policy
/// ([`quota::set_limit`]), so this gate is read-only.
fn sys_quota(buf: u64) -> u64 {
    if buf == 0 {
        return syscall_error(EFAULT);
    }
    let uid = credentials::of(task::current()).uid;
    let words = quota::stats_words(uid);
    for (index, word) in words.iter().enumerate() {
        // Safety: the caller passes a writable user buffer of
        // `quota::STATS_WORDS` eight-byte words; the native syscall ABI trusts
        // user buffers today (see `read_cred`).
        unsafe { user_ptr::write_at::<u64>(buf, index, *word) };
    }
    0
}

/// syscall 13: copy a [`task::introspect::TaskSnapshot`] scheduler snapshot
/// into the caller's buffer (MCP debug bridge Phase 2, `docs/mcp-debug-bridge.md`).
///
/// `buf` points at [`task::introspect::WORDS`] `u64`s. A null buffer is
/// `-EFAULT`; like [`sys_quota`], this gate is read-only and discloses no
/// more than `messengerctl sessions` already does.
fn sys_tasks(buf: u64) -> u64 {
    if buf == 0 {
        return syscall_error(EFAULT);
    }
    let words = task::introspect::snapshot_words();
    for (index, word) in words.iter().enumerate() {
        // Safety: the caller passes a writable user buffer of
        // `task::introspect::WORDS` eight-byte words; see `sys_quota`.
        unsafe { user_ptr::write_at::<u64>(buf, index, *word) };
    }
    0
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
    if count > 0 {
        // Safety: the caller passes a buffer valid for `buf_len` bytes (the
        // syscall ABI's contract).
        unsafe { user_ptr::copy_to(buf_ptr, &args[..count]) };
    }
    args.len() as u64
}

/// Map a program's `PT_LOAD` segments into `table` and return its entry point.
///
/// Segments are mapped eagerly (their contents must exist before the program
/// runs) with the protection the ELF header asks for: read always, write only
/// for `PF_W`, execute only for `PF_X`. Each segment is recorded as a `File`
/// VMA so `munmap`/`mprotect` and diagnostics see the same layout the hardware
/// does.
pub fn load_segments(table: PhysAddr, elf_bytes: &[u8]) -> Result<u64, &'static str> {
    let elf = ElfFile::new(elf_bytes).map_err(|_| "not a valid ELF")?;
    let entry = elf.header.pt2.entry_point();

    let mut pages: Vec<(u64, u64)> = Vec::new();
    for program_header in elf.program_iter() {
        if program_header.get_type() != Ok(ProgramType::Load) {
            continue;
        }
        let vaddr = program_header.virtual_addr();
        let mem_size = program_header.mem_size();
        let start = vaddr & !0xFFF;
        let end = (vaddr + mem_size + 0xFFF) & !0xFFF;

        let flags = program_header.flags();
        let mut prot = Prot::READ;
        if flags.is_write() {
            prot = prot | Prot::WRITE;
        }
        if flags.is_execute() {
            prot = prot | Prot::EXEC;
        }

        let mut va = start;
        while va < end {
            if phys_for(&pages, va).is_none() {
                let phys = mem::alloc_zeroed_frame().ok_or("out of memory")?;
                if !mem::map_page_in(table, VirtAddr::new(va), phys, mem::prot_flags(prot)) {
                    return Err("failed to map segment");
                }
                pages.push((va, phys.as_u64()));
            }
            va += 4096;
        }

        let data = program_header
            .get_data(&elf)
            .map_err(|_| "bad segment data")?;
        if let SegmentData::Undefined(file_bytes) = data {
            for (i, &byte) in file_bytes.iter().enumerate() {
                let va = vaddr + i as u64;
                if let Some(phys) = phys_for(&pages, va) {
                    let dst = mem::phys_to_virt(PhysAddr::new(phys)) + (va & 0xFFF);
                    // Safety: within the freshly-mapped user page.
                    unsafe { core::ptr::write_volatile(dst.as_mut_ptr::<u8>(), byte) };
                }
            }
        }

        mem::vma::insert(table, start, end, prot, Kind::File);
    }

    Ok(entry)
}

/// Load a static ELF64 image and map the native user stack.
pub fn load_image(table: PhysAddr, elf_bytes: &[u8]) -> Result<u64, &'static str> {
    let entry = load_segments(table, elf_bytes)?;
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
