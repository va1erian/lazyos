//! `fpsig` — x87/SSE state survives asynchronous signals (issue #809).
//!
//! Two threads of one process each hold a distinct pattern in XMM0-XMM7 and a
//! distinct MXCSR, and check them in a tight assembly loop that runs no
//! compiled code. The main thread aims `SIGUSR1` at them alternately
//! (`tgkill`), so the handler runs at an arbitrary instruction of a running
//! thread while its sibling is parked in the scheduler: the kernel has to
//! save the interrupted state in the signal frame, restore it on
//! `rt_sigreturn`, and take a sibling's state from its saved area when the
//! timer sweep delivers to it. The handler overwrites all sixteen XMM
//! registers and MXCSR, so any state that leaks either way is seen.

mod common;

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const SYS_RT_SIGACTION: u64 = 13;
const SYS_GETPID: u64 = 39;
const SYS_GETTID: u64 = 186;
const SYS_TGKILL: u64 = 234;
const SIGUSR1: u64 = 10;
const SA_RESTORER: u64 = 0x0400_0000;

/// Signals the handler must have run before the spinners stop.
const TARGET: u32 = 300;
/// Give up (and fail) after this long: the handler is not running.
const TIMEOUT: Duration = Duration::from_secs(30);

static COUNT: AtomicU32 = AtomicU32::new(0);
static SEEN: AtomicU32 = AtomicU32::new(0);
static TID_A: AtomicU64 = AtomicU64::new(0);
static TID_B: AtomicU64 = AtomicU64::new(0);

core::arch::global_asm!(
    r#"
    .globl __fpsig_restore_rt
    __fpsig_restore_rt:
        mov rax, 15
        syscall
"#
);

unsafe extern "C" {
    fn __fpsig_restore_rt();
}

#[repr(C)]
struct KernelSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

/// MXCSR the handler switches to: round toward zero, all exceptions masked.
static HANDLER_MXCSR: u32 = 0x7f80;

/// Overwrite every XMM register and MXCSR, then count the delivery.
extern "C" fn handler(_sig: i32) {
    // SAFETY: only vector registers and MXCSR are written, and all of them
    // are declared clobbered.
    unsafe {
        core::arch::asm!(
            "pcmpeqb xmm0, xmm0", "pcmpeqb xmm1, xmm1", "pcmpeqb xmm2, xmm2",
            "pcmpeqb xmm3, xmm3", "pcmpeqb xmm4, xmm4", "pcmpeqb xmm5, xmm5",
            "pcmpeqb xmm6, xmm6", "pcmpeqb xmm7, xmm7", "pcmpeqb xmm8, xmm8",
            "pcmpeqb xmm9, xmm9", "pcmpeqb xmm10, xmm10", "pcmpeqb xmm11, xmm11",
            "pcmpeqb xmm12, xmm12", "pcmpeqb xmm13, xmm13", "pcmpeqb xmm14, xmm14",
            "pcmpeqb xmm15, xmm15",
            "ldmxcsr [{m}]",
            m = in(reg) &HANDLER_MXCSR,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            options(nostack)
        );
    }
    COUNT.fetch_add(1, Ordering::SeqCst);
}

unsafe fn syscall1(nr: u64, a1: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") a1,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

#[repr(C, align(16))]
struct Block([u8; 128]);

/// Eight distinct 16-byte registers values for thread `seed`.
fn pattern(seed: u8) -> Block {
    let mut block = Block([0; 128]);
    for (index, byte) in block.0.iter_mut().enumerate() {
        *byte = seed.wrapping_mul(31).wrapping_add(index as u8 ^ 0x5a);
    }
    block
}

/// Hold `block` in XMM0-XMM7 and `mxcsr` in MXCSR until `COUNT` reaches
/// `TARGET`, checking all of it on every pass. Returns 0, or the 1-based
/// register that changed, 9 for MXCSR, 10 for RCX, 11 for R11.
fn spin(block: &Block, mxcsr: u32) -> (u64, u64, u64) {
    let status: u64;
    let (rcx_seen, r11_seen): (u64, u64);
    // Loaded into MXCSR on entry, then overwritten with the live value on
    // every pass.
    let mut scratch = mxcsr;
    // SAFETY: the block is 16-byte aligned for `pcmpeqb`'s memory operand;
    // the scratch word is a live local; only vector registers, `eax` and the
    // named registers are touched, and the loop calls nothing.
    unsafe {
        core::arch::asm!(
            "movdqa xmm0, [rsi]",
            "movdqa xmm1, [rsi + 16]",
            "movdqa xmm2, [rsi + 32]",
            "movdqa xmm3, [rsi + 48]",
            "movdqa xmm4, [rsi + 64]",
            "movdqa xmm5, [rsi + 80]",
            "movdqa xmm6, [rsi + 96]",
            "movdqa xmm7, [rsi + 112]",
            "ldmxcsr [{scratch}]",
            // `rcx` and `r11` are what `syscall`/`sysretq` overwrite; they
            // must come back from `rt_sigreturn` like any other register.
            "mov rcx, 0x1122334455667788",
            "mov r11, 0x0f1e2d3c4b5a6978",
            "2:",
            "mov edx, 1", "movdqa xmm8, xmm0", "pcmpeqb xmm8, [rsi]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 2", "movdqa xmm8, xmm1", "pcmpeqb xmm8, [rsi + 16]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 3", "movdqa xmm8, xmm2", "pcmpeqb xmm8, [rsi + 32]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 4", "movdqa xmm8, xmm3", "pcmpeqb xmm8, [rsi + 48]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 5", "movdqa xmm8, xmm4", "pcmpeqb xmm8, [rsi + 64]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 6", "movdqa xmm8, xmm5", "pcmpeqb xmm8, [rsi + 80]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 7", "movdqa xmm8, xmm6", "pcmpeqb xmm8, [rsi + 96]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 8", "movdqa xmm8, xmm7", "pcmpeqb xmm8, [rsi + 112]",
            "pmovmskb eax, xmm8", "cmp eax, 0xffff", "jne 3f",
            "mov edx, 9", "stmxcsr [{scratch}]", "mov eax, [{scratch}]",
            "cmp eax, r9d", "jne 3f",
            "mov edx, 10", "mov rax, 0x1122334455667788", "cmp rcx, rax", "jne 3f",
            "mov edx, 11", "mov rax, 0x0f1e2d3c4b5a6978", "cmp r11, rax", "jne 3f",
            "xor edx, edx",
            "cmp dword ptr [rdi], r8d",
            "jb 2b",
            "3:",
            scratch = in(reg) &mut scratch,
            in("rsi") block,
            in("rdi") COUNT.as_ptr(),
            in("r8") u64::from(TARGET),
            in("r9") u64::from(mxcsr),
            out("rdx") status,
            out("rax") _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _,
            out("rcx") rcx_seen, out("r11") r11_seen,
            options(nostack)
        );
    }
    (status, rcx_seen, r11_seen)
}

fn spinner(seed: u8, mxcsr: u32, tid: &'static AtomicU64) -> (u64, u64, u64) {
    let block = pattern(seed);
    // SAFETY: plain syscall with no pointers.
    tid.store(unsafe { syscall1(SYS_GETTID, 0) } as u64, Ordering::SeqCst);
    spin(&block, mxcsr)
}

fn main() {
    let action = KernelSigaction {
        handler: handler as usize,
        flags: SA_RESTORER,
        restorer: __fpsig_restore_rt as *const () as usize,
        mask: 0,
    };
    // SAFETY: `action` is a valid kernel `struct sigaction` for the call.
    let installed = unsafe {
        let ret: i64;
        core::arch::asm!(
            "syscall",
            inlateout("rax") SYS_RT_SIGACTION as i64 => ret,
            in("rdi") SIGUSR1,
            in("rsi") &action as *const _ as u64,
            in("rdx") 0u64,
            in("r10") 8u64,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
        ret
    };
    if installed != 0 {
        common::fail("fpsig", "rt_sigaction failed");
    }

    let a = std::thread::spawn(|| spinner(0x11, 0x3f80, &TID_A));
    let b = std::thread::spawn(|| spinner(0xa7, 0x5f80, &TID_B));

    let started = Instant::now();
    while TID_A.load(Ordering::SeqCst) == 0 || TID_B.load(Ordering::SeqCst) == 0 {
        if started.elapsed() > TIMEOUT {
            common::fail("fpsig", "spinner threads did not start");
        }
        std::thread::yield_now();
    }
    // SAFETY: plain syscalls with no pointers.
    let pid = unsafe { syscall1(SYS_GETPID, 0) } as u64;
    let mut turn = 0u32;
    while COUNT.load(Ordering::SeqCst) < TARGET {
        if started.elapsed() > TIMEOUT {
            // Release the spinners so the failure can be reported.
            SEEN.store(COUNT.load(Ordering::SeqCst), Ordering::SeqCst);
            COUNT.store(u32::MAX, Ordering::SeqCst);
            break;
        }
        let tid = if turn % 2 == 0 { &TID_A } else { &TID_B };
        turn += 1;
        // SAFETY: tgkill takes three integers.
        unsafe {
            syscall3(SYS_TGKILL, pid, tid.load(Ordering::SeqCst), SIGUSR1);
        }
        std::thread::sleep(Duration::from_micros(500));
    }

    let (ra, rb) = (a.join().unwrap_or((99, 0, 0)), b.join().unwrap_or((99, 0, 0)));
    let ran = COUNT.load(Ordering::SeqCst);
    // A spinner that saw its state change exits early; report that before
    // the (consequent) stall.
    if ra.0 != 0 || rb.0 != 0 {
        common::fail(
            "fpsig",
            &format!(
                "state changed under a signal: thread A {ra:x?}, thread B {rb:x?} (status, rcx, r11; 1-8 = XMM0-7, 9 = MXCSR, 10 = RCX, 11 = R11) after {} signals",
                SEEN.load(Ordering::SeqCst).max(ran.min(TARGET))
            ),
        );
    }
    if ran == u32::MAX {
        common::fail(
            "fpsig",
            &format!(
                "the signal handler stopped running after {} of {TARGET} signals",
                SEEN.load(Ordering::SeqCst)
            ),
        );
    }
    println!("ABI:fpsig:INFO:{ran} signals in {} ms", started.elapsed().as_millis());
    common::pass("fpsig");
}
