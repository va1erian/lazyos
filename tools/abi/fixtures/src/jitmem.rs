//! `jitmem` — JIT-style executable memory (issue #773): a tiny JIT that emits
//! `mov rax, N; ret` into anonymous memory and runs it, W^X flips with
//! `mprotect`, and the access errors a JIT's bugs raise, recovered through a
//! `SIGSEGV` handler that checks `si_code`/`si_addr` and resumes at a
//! recovery label (it rewrites RIP in the `ucontext`, then returns through
//! `rt_sigreturn`, the way a `siglongjmp` would).
//!
//! Covered: RW -> RX -> RW cycles for 1000 different constants; a fetch from
//! an RW page (the TLB must have lost the execute right) and a store to an RX
//! page raise `SEGV_ACCERR` with the faulting address; a fetch from or store
//! to an unmapped page is `SEGV_MAPERR`; RWX; `mprotect` over part of a
//! mapping; `mremap` keeps EXEC; the stack, the heap and `.bss` are not
//! executable until `mprotect(PROT_EXEC)` says so; a forked RX page stays RX
//! and the child's flip does not reach the parent; and a second thread
//! executes the page while the first flips it.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;
const SYS_RT_SIGACTION: u64 = 13;
const SYS_MREMAP: u64 = 25;
const SYS_FORK: u64 = 57;
const SYS_WAIT4: u64 = 61;
const SYS_EXIT_GROUP: u64 = 231;

const PROT_R: u64 = 1;
const PROT_W: u64 = 2;
const PROT_X: u64 = 4;
const RW: u64 = PROT_R | PROT_W;
const RX: u64 = PROT_R | PROT_X;
const RWX: u64 = PROT_R | PROT_W | PROT_X;
const MAP_PRIVATE_ANON: u64 = 0x22;
const MREMAP_MAYMOVE: u64 = 1;

const SIGSEGV: u64 = 11;
const SA_SIGINFO: u64 = 4;
const SA_RESTORER: u64 = 0x0400_0000;
const SEGV_MAPERR: i32 = 1;
const SEGV_ACCERR: i32 = 2;

const PAGE: usize = 4096;
/// Offset of `REG_RIP` in the handler's `ucontext_t` (`uc_flags`, `uc_link`,
/// `uc_stack`, then the general registers; RIP is the 17th).
const UC_RIP: usize = 168;

// The call and store probes keep the stack pointer in rbx, so a fault inside
// them can be recovered by resuming at a label that restores it: the handler
// only has to rewrite RIP.
core::arch::global_asm!(
    r#"
    .globl __jit_call
    __jit_call:
        push rbx
        mov rbx, rsp
        call rdi
        mov rsp, rbx
        pop rbx
        ret
    .globl __jit_call_recover
    __jit_call_recover:
        mov rsp, rbx
        pop rbx
        mov rax, -1
        ret
    .globl __jit_store
    __jit_store:
        push rbx
        mov rbx, rsp
        mov byte ptr [rdi], 0x90
        xor eax, eax
        mov rsp, rbx
        pop rbx
        ret
    .globl __jit_store_recover
    __jit_store_recover:
        mov rsp, rbx
        pop rbx
        mov eax, 1
        ret
    .globl __jit_restore_rt
    __jit_restore_rt:
        mov rax, 15
        syscall
"#
);

unsafe extern "C" {
    fn __jit_call(code: usize) -> u64;
    fn __jit_call_recover();
    fn __jit_store(addr: usize) -> u64;
    fn __jit_store_recover();
    fn __jit_restore_rt();
}

unsafe fn syscall6(nr: u64, a: [u64; 6]) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") a[0],
        in("rsi") a[1],
        in("rdx") a[2],
        in("r10") a[3],
        in("r8") a[4],
        in("r9") a[5],
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

fn mmap(len: usize, prot: u64) -> usize {
    let got = unsafe { syscall6(SYS_MMAP, [0, len as u64, prot, MAP_PRIVATE_ANON, u64::MAX, 0]) };
    if got <= 0 {
        common::fail("jitmem", &format!("mmap({len}, {prot}) = {got}"));
    }
    got as usize
}

fn mprotect(addr: usize, len: usize, prot: u64) -> i64 {
    unsafe { syscall6(SYS_MPROTECT, [addr as u64, len as u64, prot, 0, 0, 0]) }
}

fn munmap(addr: usize, len: usize) -> i64 {
    unsafe { syscall6(SYS_MUNMAP, [addr as u64, len as u64, 0, 0, 0, 0]) }
}

#[repr(C)]
struct KernelSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

static FAULTS: AtomicU64 = AtomicU64::new(0);
static LAST_CODE: AtomicU64 = AtomicU64::new(0);
static LAST_ADDR: AtomicU64 = AtomicU64::new(0);
/// Where the handler resumes: the recovery label of the probe in flight.
static RECOVER: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_segv(_sig: i32, info: *const u8, ucontext: *mut u8) {
    // SAFETY: the kernel passes a `siginfo_t` (si_code at +8, si_addr at +16)
    // and a `ucontext_t` whose saved RIP at `UC_RIP` is restored on return.
    unsafe {
        LAST_CODE.store(*(info.add(8) as *const i32) as u32 as u64, Ordering::SeqCst);
        LAST_ADDR.store(*(info.add(16) as *const u64), Ordering::SeqCst);
        *(ucontext.add(UC_RIP) as *mut usize) = RECOVER.load(Ordering::SeqCst);
    }
    FAULTS.fetch_add(1, Ordering::SeqCst);
}

fn install_handler() -> i64 {
    let action = KernelSigaction {
        handler: on_segv as *const () as usize,
        flags: SA_SIGINFO | SA_RESTORER,
        restorer: __jit_restore_rt as *const () as usize,
        mask: 0,
    };
    unsafe {
        syscall6(
            SYS_RT_SIGACTION,
            [SIGSEGV, &action as *const _ as u64, 0, 8, 0, 0],
        )
    }
}

/// What a probe saw: `Some((si_code, si_addr))` when it faulted.
type Fault = Option<(i32, u64)>;

fn probe(resume: usize, run: impl FnOnce()) -> Fault {
    RECOVER.store(resume, Ordering::SeqCst);
    let before = FAULTS.load(Ordering::SeqCst);
    run();
    (FAULTS.load(Ordering::SeqCst) != before).then(|| {
        (
            LAST_CODE.load(Ordering::SeqCst) as i32,
            LAST_ADDR.load(Ordering::SeqCst),
        )
    })
}

/// Execute at `addr`, recovering from a fault.
fn try_call(addr: usize) -> Fault {
    probe(__jit_call_recover as *const () as usize, || {
        unsafe { __jit_call(addr) };
    })
}

/// Store one byte at `addr`, recovering from a fault.
fn try_store(addr: usize) -> Fault {
    probe(__jit_store_recover as *const () as usize, || {
        unsafe { __jit_store(addr) };
    })
}

/// `mov rax, imm; ret`: the sign-extended `imm32` form for small constants,
/// `movabs` for the rest. Returns the length.
fn emit(at: usize, value: u64) -> usize {
    let mut code = [0u8; 11];
    let len = if value < (1 << 31) {
        code[..3].copy_from_slice(&[0x48, 0xC7, 0xC0]);
        code[3..7].copy_from_slice(&(value as u32).to_le_bytes());
        code[7] = 0xC3;
        8
    } else {
        code[..2].copy_from_slice(&[0x48, 0xB8]);
        code[2..10].copy_from_slice(&value.to_le_bytes());
        code[10] = 0xC3;
        11
    };
    for (i, byte) in code[..len].iter().enumerate() {
        unsafe { ((at + i) as *mut u8).write_volatile(*byte) };
    }
    len
}

fn call(addr: usize) -> u64 {
    unsafe { __jit_call(addr) }
}

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

fn expect_fault(first: &mut Option<String>, what: &str, got: Fault, code: i32, addr: usize) {
    match got {
        Some((c, a)) if c == code && a == addr as u64 => {}
        other => note(
            first,
            format!("{what}: fault {other:x?}, wanted code {code} at {addr:#x}"),
        ),
    }
}

/// A distinct constant per iteration, small and large.
fn constant(n: u64) -> u64 {
    if n % 3 == 0 {
        n.wrapping_mul(0x9E37_79B9_7F4A_7C15) | (1 << 40)
    } else {
        (n * 2_654_435 + 17) & 0x7FFF_FFFF
    }
}

/// 1000 constants through RW -> write -> RX -> call -> RW; every fifth also
/// proves the RX -> RW flip took execute away (the fetch must fault).
fn wx_cycles(first: &mut Option<String>) {
    let page = mmap(PAGE, RW);
    for n in 0..1000u64 {
        let slot = page + (n as usize % 64) * 64;
        let want = constant(n);
        emit(slot, want);
        if mprotect(page, PAGE, RX) != 0 {
            return note(first, format!("mprotect(RX) failed at {n}"));
        }
        let got = call(slot);
        if got != want {
            return note(first, format!("jit {n}: got {got:#x}, want {want:#x}"));
        }
        if n % 5 == 0 {
            expect_fault(first, "store to RX", try_store(slot), SEGV_ACCERR, slot);
            if call(slot) != want {
                note(first, format!("refused store changed the code at {n}"));
            }
        }
        if mprotect(page, PAGE, RW) != 0 {
            return note(first, format!("mprotect(RW) failed at {n}"));
        }
        if n % 5 == 0 {
            // The TLB held this page as executable a moment ago.
            expect_fault(first, "fetch from RW", try_call(slot), SEGV_ACCERR, slot);
        }
    }
    munmap(page, PAGE);
}

/// Fresh, unmapped, RWX, partial `mprotect` and `mremap`.
fn rights(first: &mut Option<String>) {
    // A page nobody has touched: the first fetch materializes it NX, the
    // retried one is the access error (Linux reports ACCERR for both).
    let fresh = mmap(PAGE, RW);
    expect_fault(first, "fetch from fresh RW", try_call(fresh), SEGV_ACCERR, fresh);
    munmap(fresh, PAGE);
    expect_fault(first, "fetch from unmapped", try_call(fresh), SEGV_MAPERR, fresh);
    expect_fault(first, "store to unmapped", try_store(fresh), SEGV_MAPERR, fresh);

    // RWX: patch in place, no mprotect.
    let rwx = mmap(PAGE, RWX);
    emit(rwx, 11);
    let before = call(rwx);
    emit(rwx, 12);
    if before != 11 || call(rwx) != 12 {
        note(first, "RWX page did not run patched code".to_string());
    }
    if try_store(rwx + 100).is_some() {
        note(first, "store to RWX faulted".to_string());
    }
    munmap(rwx, PAGE);

    // Part of a mapping: pages 1 and 2 of 4 become RX.
    let four = mmap(4 * PAGE, RW);
    for i in 0..4 {
        emit(four + i * PAGE, 100 + i as u64);
    }
    if mprotect(four + PAGE, 2 * PAGE, RX) != 0 {
        note(first, "partial mprotect failed".to_string());
    }
    for i in [1usize, 2] {
        let page = four + i * PAGE;
        if call(page) != 100 + i as u64 {
            note(first, format!("partial RX page {i} did not run"));
        }
        expect_fault(first, "store to partial RX", try_store(page), SEGV_ACCERR, page);
    }
    for i in [0usize, 3] {
        let page = four + i * PAGE;
        expect_fault(first, "fetch from the RW edge", try_call(page), SEGV_ACCERR, page);
        if try_store(page + 64).is_some() {
            note(first, format!("store to the RW edge page {i} faulted"));
        }
    }
    munmap(four, 4 * PAGE);

    // mremap keeps EXEC (and keeps write away).
    let old = mmap(PAGE, RW);
    emit(old, 77);
    mprotect(old, PAGE, RX);
    let moved = unsafe { syscall6(SYS_MREMAP, [old as u64, PAGE as u64, 3 * PAGE as u64, MREMAP_MAYMOVE, 0, 0]) };
    if moved <= 0 {
        return note(first, format!("mremap = {moved}"));
    }
    let moved = moved as usize;
    if call(moved) != 77 {
        note(first, "the code does not run after mremap".to_string());
    }
    expect_fault(first, "store after mremap", try_store(moved), SEGV_ACCERR, moved);
    munmap(moved, 3 * PAGE);
}

// Mutable, so it is zero-initialised `.bss` and not a constant the linker may
// place beside the code.
static mut BSS: [u8; 2 * PAGE] = [0; 2 * PAGE];

/// The stack, the heap and `.bss` start non-executable; `mprotect(EXEC)` on a
/// page of the stack or heap is honoured.
fn nx_by_default(first: &mut Option<String>) {
    let mut stack = [0u8; 3 * PAGE];
    let stack_page = (stack.as_mut_ptr() as usize + PAGE) & !(PAGE - 1);
    emit(stack_page, 5);
    expect_fault(first, "fetch from the stack", try_call(stack_page), SEGV_ACCERR, stack_page);
    if mprotect(stack_page, PAGE, RWX) != 0 {
        note(first, "mprotect(EXEC) on the stack failed".to_string());
    } else if call(stack_page) != 5 {
        note(first, "stack code did not run after mprotect(EXEC)".to_string());
    }
    mprotect(stack_page, PAGE, RW);
    std::hint::black_box(&mut stack);

    let mut heap = vec![0u8; 3 * PAGE];
    let heap_page = (heap.as_mut_ptr() as usize + PAGE) & !(PAGE - 1);
    emit(heap_page, 6);
    expect_fault(first, "fetch from the heap", try_call(heap_page), SEGV_ACCERR, heap_page);
    if mprotect(heap_page, PAGE, RWX) != 0 {
        note(first, "mprotect(EXEC) on the heap failed".to_string());
    } else if call(heap_page) != 6 {
        note(first, "heap code did not run after mprotect(EXEC)".to_string());
    }
    mprotect(heap_page, PAGE, RW);
    drop(heap);

    let bss = std::ptr::addr_of!(BSS) as usize;
    expect_fault(first, "fetch from .bss", try_call(bss), SEGV_ACCERR, bss);
}

/// A forked RX page is RX in the child, a store to it is refused (not copied),
/// and the child's RW -> patch -> RX flip never reaches the parent.
fn fork_cow(first: &mut Option<String>) {
    let page = mmap(PAGE, RW);
    emit(page, 1234);
    mprotect(page, PAGE, RX);
    let pid = unsafe { syscall6(SYS_FORK, [0; 6]) };
    if pid < 0 {
        return note(first, format!("fork = {pid}"));
    }
    if pid == 0 {
        let code = (|| {
            if call(page) != 1234 {
                return 10;
            }
            match try_store(page) {
                Some((SEGV_ACCERR, a)) if a == page as u64 => {}
                _ => return 11,
            }
            if mprotect(page, PAGE, RW) != 0 {
                return 12;
            }
            emit(page, 5678);
            if mprotect(page, PAGE, RX) != 0 {
                return 13;
            }
            if call(page) != 5678 {
                return 14;
            }
            0
        })();
        unsafe { syscall6(SYS_EXIT_GROUP, [code, 0, 0, 0, 0, 0]) };
        loop {}
    }
    let mut status = 0i32;
    let waited = unsafe { syscall6(SYS_WAIT4, [pid as u64, &mut status as *mut i32 as u64, 0, 0, 0, 0]) };
    if waited != pid || status != 0 {
        note(first, format!("fork child: wait4 {waited}, status {status:#x} (code {})", status >> 8));
    }
    if call(page) != 1234 {
        note(first, "the child's patch reached the parent".to_string());
    }
    expect_fault(first, "parent store to RX after fork", try_store(page), SEGV_ACCERR, page);
    // The parent can still flip and patch its own copy.
    if mprotect(page, PAGE, RW) != 0 {
        note(first, "parent mprotect(RW) after fork failed".to_string());
    }
    emit(page, 4321);
    mprotect(page, PAGE, RX);
    if call(page) != 4321 {
        note(first, "parent patch after fork did not run".to_string());
    }
    munmap(page, PAGE);
}

static WANT_RW: AtomicBool = AtomicBool::new(false);
static INSIDE: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static GEN: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static WRONG: AtomicU64 = AtomicU64::new(0);

const THREAD_FLIPS: u64 = 10_000;

/// One thread flips the page RW -> patch -> RX while another executes it; the
/// runner enters only outside the flipper's window and must see the current
/// constant every time.
fn flip_under_thread(first: &mut Option<String>) {
    let page = mmap(PAGE, RW);
    emit(page, 0);
    mprotect(page, PAGE, RX);
    GEN.store(0, Ordering::SeqCst);
    READY.store(true, Ordering::SeqCst);
    let runner = std::thread::spawn(move || {
        while !STOP.load(Ordering::SeqCst) {
            if WANT_RW.load(Ordering::SeqCst) || !READY.load(Ordering::SeqCst) {
                std::thread::yield_now();
                continue;
            }
            INSIDE.store(true, Ordering::SeqCst);
            if WANT_RW.load(Ordering::SeqCst) {
                INSIDE.store(false, Ordering::SeqCst);
                continue;
            }
            let expect = GEN.load(Ordering::SeqCst);
            let got = call(page);
            INSIDE.store(false, Ordering::SeqCst);
            if got != expect {
                WRONG.fetch_add(1, Ordering::SeqCst);
            }
            if CALLS.fetch_add(1, Ordering::SeqCst) % 16 == 15 {
                std::thread::yield_now();
            }
        }
    });
    for flip in 1..=THREAD_FLIPS {
        WANT_RW.store(true, Ordering::SeqCst);
        while INSIDE.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        if mprotect(page, PAGE, RW) != 0 {
            note(first, format!("thread flip {flip}: mprotect(RW) failed"));
            break;
        }
        emit(page, constant(flip));
        if mprotect(page, PAGE, RX) != 0 {
            note(first, format!("thread flip {flip}: mprotect(RX) failed"));
            break;
        }
        GEN.store(constant(flip), Ordering::SeqCst);
        WANT_RW.store(false, Ordering::SeqCst);
        if flip % 8 == 0 {
            std::thread::yield_now();
        }
    }
    WANT_RW.store(true, Ordering::SeqCst);
    STOP.store(true, Ordering::SeqCst);
    let _ = runner.join();
    let (calls, wrong) = (CALLS.load(Ordering::SeqCst), WRONG.load(Ordering::SeqCst));
    if wrong != 0 {
        note(first, format!("{wrong} of {calls} threaded calls saw stale code"));
    }
    if calls < THREAD_FLIPS / 16 {
        note(first, format!("the runner thread only made {calls} calls"));
    }
    munmap(page, PAGE);
}

fn main() {
    let mut first: Option<String> = None;
    if install_handler() != 0 {
        common::fail("jitmem", "rt_sigaction(SIGSEGV) failed");
    }
    wx_cycles(&mut first);
    rights(&mut first);
    nx_by_default(&mut first);
    fork_cow(&mut first);
    flip_under_thread(&mut first);
    match first {
        Some(reason) => common::fail("jitmem", &reason),
        None => common::pass("jitmem"),
    }
}
