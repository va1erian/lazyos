//! Helpers of the `jitmem` fixture: raw syscalls, the `SIGSEGV` recovery
//! handler and the call/store probes (see `jitmem.rs`).

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const SYS_MMAP: u64 = 9;
pub const SYS_MPROTECT: u64 = 10;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_RT_SIGACTION: u64 = 13;
pub const SYS_MREMAP: u64 = 25;
pub const SYS_FORK: u64 = 57;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_EXIT_GROUP: u64 = 231;

pub const PROT_R: u64 = 1;
pub const PROT_W: u64 = 2;
pub const PROT_X: u64 = 4;
pub const RW: u64 = PROT_R | PROT_W;
pub const RX: u64 = PROT_R | PROT_X;
pub const RWX: u64 = PROT_R | PROT_W | PROT_X;
pub const MAP_PRIVATE_ANON: u64 = 0x22;
pub const MREMAP_MAYMOVE: u64 = 1;

pub const SIGSEGV: u64 = 11;
pub const SA_SIGINFO: u64 = 4;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;

pub const PAGE: usize = 4096;
/// Offset of `REG_RIP` in the handler's `ucontext_t` (`uc_flags`, `uc_link`,
/// `uc_stack`, then the general registers; RIP is the 17th).
pub const UC_RIP: usize = 168;

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

pub unsafe fn syscall6(nr: u64, a: [u64; 6]) -> i64 {
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

pub fn mmap(len: usize, prot: u64) -> usize {
    let got = unsafe {
        syscall6(
            SYS_MMAP,
            [0, len as u64, prot, MAP_PRIVATE_ANON, u64::MAX, 0],
        )
    };
    if got <= 0 {
        crate::common::fail("jitmem", &format!("mmap({len}, {prot}) = {got}"));
    }
    got as usize
}

pub fn mprotect(addr: usize, len: usize, prot: u64) -> i64 {
    unsafe { syscall6(SYS_MPROTECT, [addr as u64, len as u64, prot, 0, 0, 0]) }
}

pub fn munmap(addr: usize, len: usize) -> i64 {
    unsafe { syscall6(SYS_MUNMAP, [addr as u64, len as u64, 0, 0, 0, 0]) }
}

#[repr(C)]
pub struct KernelSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

pub static FAULTS: AtomicU64 = AtomicU64::new(0);
pub static LAST_CODE: AtomicU64 = AtomicU64::new(0);
pub static LAST_ADDR: AtomicU64 = AtomicU64::new(0);
/// Where the handler resumes: the recovery label of the probe in flight.
pub static RECOVER: AtomicUsize = AtomicUsize::new(0);

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

pub fn install_handler() -> i64 {
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
pub type Fault = Option<(i32, u64)>;

pub fn probe(resume: usize, run: impl FnOnce()) -> Fault {
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
pub fn try_call(addr: usize) -> Fault {
    probe(__jit_call_recover as *const () as usize, || {
        unsafe { __jit_call(addr) };
    })
}

/// Store one byte at `addr`, recovering from a fault.
pub fn try_store(addr: usize) -> Fault {
    probe(__jit_store_recover as *const () as usize, || {
        unsafe { __jit_store(addr) };
    })
}

/// `mov rax, imm; ret`: the sign-extended `imm32` form for small constants,
/// `movabs` for the rest. Returns the length.
pub fn emit(at: usize, value: u64) -> usize {
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

pub fn call(addr: usize) -> u64 {
    unsafe { __jit_call(addr) }
}

pub fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

pub fn expect_fault(first: &mut Option<String>, what: &str, got: Fault, code: i32, addr: usize) {
    match got {
        Some((c, a)) if c == code && a == addr as u64 => {}
        other => note(
            first,
            format!("{what}: fault {other:x?}, wanted code {code} at {addr:#x}"),
        ),
    }
}

/// A distinct constant per iteration, small and large.
pub fn constant(n: u64) -> u64 {
    if n % 3 == 0 {
        n.wrapping_mul(0x9E37_79B9_7F4A_7C15) | (1 << 40)
    } else {
        (n * 2_654_435 + 17) & 0x7FFF_FFFF
    }
}
