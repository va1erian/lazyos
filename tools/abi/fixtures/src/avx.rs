//! `avx` — AVX state: enabled, kept per thread, saved in signal frames,
//! copied by `fork` and reset by `execve` (issue #822).
//!
//! `CPUID` reports AVX whatever the OS did, so the first check is the one the
//! manual prescribes: `OSXSAVE` set and `XCR0` naming x87, SSE and AVX (and
//! no AVX-512, whose state the kernel does not save). Then:
//!
//! 1. VEX instructions execute (the `vmovsd [r11+0x50], xmm0` that killed the
//!    JavaScriptCore JIT shell with `#UD`).
//! 2. Two threads each hold a distinct 256-bit pattern in YMM0-YMM7 and check
//!    all of it in a tight assembly loop that runs no compiled code, while the
//!    timer preempts them and the main thread aims `SIGUSR1` at them
//!    (`tgkill`). The handler overwrites YMM0-YMM15 and checks it started from
//!    the initial state (Linux hands a handler clean registers), so a lost
//!    context switch, a state that leaks into the handler, or one that the
//!    frame fails to restore, is seen.
//! 3. A handler reads the signal frame the way musl, glibc and libgcc do:
//!    `uc_mcontext.fpstate`, `FP_XSTATE_MAGIC1` and the size in the software
//!    bytes, `FP_XSTATE_MAGIC2` after the area, and the interrupted upper YMM
//!    halves at offset 576; it rewrites them, and the interrupted code must
//!    see the edit after `rt_sigreturn`.
//! 4. `fork`: the child starts with the parent's YMM halves and changing them
//!    does not reach the parent.
//! 5. `execve`: a new program starts with the initial AVX state.
//!
//! On a CPU without AVX (QEMU's default `qemu64` under TCG) it prints
//! `ABI:avx:INFO:...` and passes: run the bench with `--cpu max` to run it.
//! Only the upper halves are checked between assembly blocks: compiled code
//! uses legacy SSE, which may change the low 128 bits but never the upper.

mod common;

use std::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};
use std::process::Command;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const SYS_RT_SIGACTION: u64 = 13;
const SYS_GETPID: u64 = 39;
const SYS_FORK: u64 = 57;
const SYS_WAIT4: u64 = 61;
const SYS_GETTID: u64 = 186;
const SYS_TGKILL: u64 = 234;
const SYS_EXIT_GROUP: u64 = 231;
const SIGUSR1: u64 = 10;
const SIGUSR2: u64 = 12;
const SA_SIGINFO: u64 = 4;
const SA_RESTORER: u64 = 0x0400_0000;

/// The program's own name on the image, which `Command` finds on `$PATH`.
const PROGRAM: &str = "abi-init";
const EXEC_CHILD: &str = "avx-exec-child";

/// Signals `handler` must have run before the spinners stop.
const TARGET: u32 = 300;
const TIMEOUT: Duration = Duration::from_secs(30);
/// `FP_XSTATE_MAGIC1` / `FP_XSTATE_MAGIC2` of `<asm/sigcontext.h>`.
const MAGIC1: u32 = 0x4650_5853;
const MAGIC2: u32 = 0x4650_5845;

static COUNT: AtomicU32 = AtomicU32::new(0);
static DIRTY_START: AtomicU32 = AtomicU32::new(0);
static TID_A: AtomicU64 = AtomicU64::new(0);
static TID_B: AtomicU64 = AtomicU64::new(0);
/// 0 until `handler_edit` ran, then 1 + the first thing it found wrong.
static EDIT: AtomicU32 = AtomicU32::new(0);
static EDIT_SIZE: AtomicU32 = AtomicU32::new(0);

core::arch::global_asm!(
    r#"
    .globl __avx_restore_rt
    __avx_restore_rt:
        mov rax, 15
        syscall
"#
);

unsafe extern "C" {
    fn __avx_restore_rt();
}

#[repr(C)]
struct KernelSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}


mod avxops;
use avxops::{check_ymm_hi, dirty_all_ymm, set_ymm_hi, block, hi, spin, Block, Hi, STOP};


unsafe fn syscall6(nr: u64, a: [u64; 6]) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") a[0], in("rsi") a[1], in("rdx") a[2],
        in("r10") a[3], in("r8") a[4], in("r9") a[5],
        lateout("rcx") _, lateout("r11") _,
        options(nostack)
    );
    ret
}


/// Handler for `SIGUSR1`: note whether it started from the initial state
/// (upper halves of YMM0-YMM7 all zero), overwrite every YMM register, count.
extern "C" fn handler(_sig: i32) {
    let leaked: u32;
    // SAFETY: only vector registers and `eax` are written, all declared.
    unsafe {
        core::arch::asm!(
            "vextractf128 xmm8, ymm0, 1", "vextractf128 xmm9, ymm1, 1",
            "vpor xmm8, xmm8, xmm9",
            "vextractf128 xmm9, ymm2, 1", "vpor xmm8, xmm8, xmm9",
            "vextractf128 xmm9, ymm3, 1", "vpor xmm8, xmm8, xmm9",
            "vextractf128 xmm9, ymm4, 1", "vpor xmm8, xmm8, xmm9",
            "vextractf128 xmm9, ymm5, 1", "vpor xmm8, xmm8, xmm9",
            "vextractf128 xmm9, ymm6, 1", "vpor xmm8, xmm8, xmm9",
            "vextractf128 xmm9, ymm7, 1", "vpor xmm8, xmm8, xmm9",
            "xor eax, eax", "vptest xmm8, xmm8", "setnz al",
            out("eax") leaked,
            out("xmm8") _, out("xmm9") _,
            options(nostack)
        );
    }
    DIRTY_START.fetch_add(leaked, Ordering::SeqCst);
    dirty_all_ymm();
    COUNT.fetch_add(1, Ordering::SeqCst);
}

/// Handler for `SIGUSR2` (`SA_SIGINFO`): read the frame's `XSAVE` area as
/// libgcc and debuggers do, check the interrupted upper YMM halves, replace
/// them with `hi(0x77)`.
extern "C" fn handler_edit(_sig: i32, _info: *mut u8, ucontext: *mut u8) {
    let expected = hi(0x66);
    let replacement = hi(0x77);
    let mut problem = 0u32;
    // SAFETY: `ucontext` is the `ucontext_t` the kernel built; its
    // `uc_mcontext.fpstate` (offset 224) points at the frame's FP state, which
    // holds the markers and the area the reads below stay inside.
    unsafe {
        let fp = (ucontext.add(224) as *const u64).read() as *mut u8;
        if fp.is_null() || (fp as usize) % 64 != 0 {
            problem = 1;
        } else {
            let magic1 = (fp.add(464) as *const u32).read();
            let extended = (fp.add(468) as *const u32).read();
            let features = (fp.add(472) as *const u64).read();
            let size = (fp.add(480) as *const u32).read();
            EDIT_SIZE.store(size, Ordering::SeqCst);
            if magic1 != MAGIC1 {
                problem = 2;
            } else if size < 832 || size > 4096 || extended != size + 4 {
                problem = 3;
            } else if features & 6 != 6 {
                problem = 4;
            } else if (fp.add(size as usize) as *const u32).read() != MAGIC2 {
                problem = 5;
            } else if (fp.add(512) as *const u64).read() & 4 == 0 {
                problem = 6; // XSTATE_BV: AVX state in use
            } else {
                for (index, chunk) in expected.0.chunks(16).enumerate() {
                    let slot = fp.add(576 + 16 * index);
                    let got = core::slice::from_raw_parts(slot, 16);
                    if got != chunk {
                        problem = 7 + index as u32;
                        break;
                    }
                }
                for (index, chunk) in replacement.0.chunks(16).enumerate() {
                    core::ptr::copy_nonoverlapping(chunk.as_ptr(), fp.add(576 + 16 * index), 16);
                }
            }
        }
    }
    EDIT.store(1 + problem, Ordering::SeqCst);
}

fn install(signal: u64, handler: usize, flags: u64) -> i64 {
    let action = KernelSigaction {
        handler,
        flags: flags | SA_RESTORER,
        restorer: __avx_restore_rt as *const () as usize,
        mask: 0,
    };
    // SAFETY: `action` is a valid kernel `struct sigaction` for the call.
    unsafe {
        syscall6(
            SYS_RT_SIGACTION,
            [signal, &action as *const _ as u64, 0, 8, 0, 0],
        )
    }
}



fn spinner(seed: u8, tid: &'static AtomicU64) -> u64 {
    let block = block(seed);
    // SAFETY: plain syscall with no pointers.
    tid.store(unsafe { syscall6(SYS_GETTID, [0; 6]) } as u64, Ordering::SeqCst);
    spin(&block)
}

/// What `CPUID` and `XGETBV` say; `Err` is a failure, `Ok(false)` no AVX.
fn probe() -> Result<bool, String> {
    let leaf1 = __cpuid(1);
    let (xsave, osxsave, avx) = (
        leaf1.ecx & (1 << 26) != 0,
        leaf1.ecx & (1 << 27) != 0,
        leaf1.ecx & (1 << 28) != 0,
    );
    println!("ABI:avx:INFO:cpuid xsave={xsave} osxsave={osxsave} avx={avx}");
    if !avx {
        println!("ABI:avx:INFO:no AVX on this CPU (QEMU's qemu64? try --cpu max); nothing to check");
        return Ok(false);
    }
    if !(xsave && osxsave) {
        return Err("CPUID advertises AVX but the OS did not set OSXSAVE".into());
    }
    // SAFETY: OSXSAVE is set, so XGETBV does not fault.
    let xcr0 = unsafe { _xgetbv(0) };
    let size = __cpuid_count(0xd, 0).ebx;
    println!("ABI:avx:INFO:xcr0={xcr0:#x} xsave_size={size}");
    if xcr0 & 7 != 7 {
        return Err(format!("XCR0 is {xcr0:#x}: x87, SSE and AVX must be enabled"));
    }
    if xcr0 & 0xe0 != 0 {
        return Err(format!("XCR0 {xcr0:#x} enables AVX-512 state the kernel does not save"));
    }
    Ok(true)
}

/// VEX instructions execute and compute.
fn vex_smoke() -> Result<(), String> {
    let a = [1.5f64, -2.25, 8.0, 0.125];
    let mut sum = [0f64; 4];
    // SAFETY: loads and stores 32 bytes each; YMM0 and YMM1 are clobbered.
    unsafe {
        core::arch::asm!(
            "vmovupd ymm0, [{a}]", "vaddpd ymm1, ymm0, ymm0", "vmovupd [{s}], ymm1",
            a = in(reg) a.as_ptr(), s = in(reg) sum.as_mut_ptr(),
            out("xmm0") _, out("xmm1") _,
            options(nostack)
        );
    }
    if sum != [3.0, -4.5, 16.0, 0.25] {
        return Err(format!("vaddpd gave {sum:?}"));
    }
    // `c4 c1 7b 11 43 50`: vmovsd [r11+0x50], xmm0.
    let mut cell = [0u64; 2];
    let base = (cell.as_mut_ptr() as usize).wrapping_sub(0x50);
    // SAFETY: stores 8 bytes to `base + 0x50`, which is `cell[0]`; XMM0 is
    // clobbered.
    unsafe {
        core::arch::asm!(
            "vmovsd xmm0, qword ptr [{a}]", "vmovsd [{b} + 0x50], xmm0",
            a = in(reg) a.as_ptr(), b = in(reg) base,
            out("xmm0") _,
            options(nostack)
        );
    }
    if f64::from_bits(cell[0]) != a[0] {
        return Err("vmovsd [mem+0x50], xmm0 stored the wrong value".into());
    }
    Ok(())
}

/// A handler that edits the saved upper halves, and the markers it reads.
fn frame_edit() -> Result<(), String> {
    if install(SIGUSR2, handler_edit as usize, SA_SIGINFO) != 0 {
        return Err("rt_sigaction(SIGUSR2) failed".into());
    }
    // SAFETY: plain syscalls with no pointers.
    let (pid, tid) = unsafe { (syscall6(SYS_GETPID, [0; 6]), syscall6(SYS_GETTID, [0; 6])) };
    set_ymm_hi(&hi(0x66));
    // SAFETY: tgkill takes three integers.
    unsafe { syscall6(SYS_TGKILL, [pid as u64, tid as u64, SIGUSR2, 0, 0, 0]) };
    let after = check_ymm_hi(&hi(0x77));
    let edit = EDIT.load(Ordering::SeqCst);
    let size = EDIT_SIZE.load(Ordering::SeqCst);
    if edit == 0 {
        return Err("the SIGUSR2 handler did not run".into());
    }
    if edit != 1 {
        return Err(format!(
            "signal frame check {} failed (xstate_size {size}; 2 = magic1, 3 = sizes, \
             4 = xfeatures, 5 = magic2, 6 = XSTATE_BV, 7.. = YMM0..7 upper half)",
            edit - 1
        ));
    }
    if after != 0 {
        return Err(format!(
            "the handler's edit of the frame was not restored: YMM{} upper half",
            after - 1
        ));
    }
    Ok(())
}

/// The child starts with the parent's upper halves; its changes stay its own.
fn fork_copy() -> Result<(), String> {
    let pattern = hi(0x31);
    set_ymm_hi(&pattern);
    // SAFETY: fork takes no arguments; the child only runs `check_ymm_hi`,
    // `dirty_all_ymm` and `exit_group`.
    let pid = unsafe { syscall6(SYS_FORK, [0; 6]) };
    if pid < 0 {
        return Err(format!("fork = {pid}"));
    }
    if pid == 0 {
        let code = check_ymm_hi(&pattern);
        dirty_all_ymm();
        // SAFETY: exit_group takes one integer and does not return.
        unsafe { syscall6(SYS_EXIT_GROUP, [code, 0, 0, 0, 0, 0]) };
        loop {}
    }
    let mut status = 0i32;
    // SAFETY: `status` is a live local the kernel writes 4 bytes to.
    let waited = unsafe {
        syscall6(SYS_WAIT4, [pid as u64, &mut status as *mut i32 as u64, 0, 0, 0, 0])
    };
    if waited != pid {
        return Err(format!("wait4 = {waited}"));
    }
    if status != 0 {
        return Err(format!(
            "the forked child saw a different upper half (status {status:#x}, \
             YMM{} first)",
            (status >> 8).saturating_sub(1)
        ));
    }
    let after = check_ymm_hi(&pattern);
    if after != 0 {
        return Err(format!("the child's YMM changes reached the parent (YMM{})", after - 1));
    }
    Ok(())
}

/// A new program starts with the initial AVX state.
fn exec_reset() -> Result<(), String> {
    let pattern = hi(0x52);
    set_ymm_hi(&pattern);
    let status = Command::new(PROGRAM)
        .arg(EXEC_CHILD)
        .status()
        .map_err(|error| format!("spawn {PROGRAM}: {error}"))?;
    match status.code() {
        Some(0) => {}
        Some(code) => return Err(format!("the exec'd program inherited YMM{} (upper half)", code - 1)),
        None => return Err(format!("the exec'd program ended with {status:?}")),
    }
    let after = check_ymm_hi(&pattern);
    if after != 0 {
        return Err(format!("the exec'd program's run changed the parent's YMM{}", after - 1));
    }
    Ok(())
}

fn run() -> Result<(), String> {
    if !probe()? {
        return Ok(());
    }
    vex_smoke()?;
    println!("ABI:avx:STEP:vex");
    if install(SIGUSR1, handler as usize, 0) != 0 {
        return Err("rt_sigaction(SIGUSR1) failed".into());
    }
    frame_edit()?;
    println!("ABI:avx:STEP:frame");
    fork_copy()?;
    println!("ABI:avx:STEP:fork");
    exec_reset()?;
    println!("ABI:avx:STEP:exec");

    let a = std::thread::spawn(|| spinner(0x11, &TID_A));
    let b = std::thread::spawn(|| spinner(0xa7, &TID_B));
    let started = Instant::now();
    while TID_A.load(Ordering::SeqCst) == 0 || TID_B.load(Ordering::SeqCst) == 0 {
        if started.elapsed() > TIMEOUT {
            STOP.store(1, Ordering::SeqCst);
            return Err("spinner threads did not start".into());
        }
        std::thread::yield_now();
    }
    // A stretch of pure preemption first: two spinners and this thread.
    std::thread::sleep(Duration::from_millis(300));
    // SAFETY: plain syscall with no pointers.
    let pid = unsafe { syscall6(SYS_GETPID, [0; 6]) } as u64;
    let mut turn = 0u32;
    while COUNT.load(Ordering::SeqCst) < TARGET && started.elapsed() <= TIMEOUT {
        let tid = if turn % 2 == 0 { &TID_A } else { &TID_B };
        turn += 1;
        // SAFETY: tgkill takes three integers.
        unsafe { syscall6(SYS_TGKILL, [pid, tid.load(Ordering::SeqCst), SIGUSR1, 0, 0, 0]) };
        std::thread::sleep(Duration::from_micros(500));
    }
    STOP.store(1, Ordering::SeqCst);
    let (ra, rb) = (a.join().unwrap_or(99), b.join().unwrap_or(99));
    let ran = COUNT.load(Ordering::SeqCst);
    if ra != 0 || rb != 0 {
        return Err(format!(
            "YMM state changed under preemption or a signal: thread A status {ra}, thread B \
             status {rb} (0 = intact, 1-8 = YMM0-7 changed) after {ran} signals"
        ));
    }
    if ran < TARGET {
        return Err(format!("only {ran} of {TARGET} signals were handled"));
    }
    let leaked = DIRTY_START.load(Ordering::SeqCst);
    if leaked != 0 {
        return Err(format!(
            "{leaked} handlers started with the interrupted code's upper YMM halves"
        ));
    }
    println!("ABI:avx:INFO:{ran} signals in {} ms", started.elapsed().as_millis());
    Ok(())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(EXEC_CHILD) {
        // A fresh program: every upper half must be zero.
        std::process::exit(check_ymm_hi(&Hi([0; 128])) as i32);
    }
    match run() {
        Ok(()) => common::pass("avx"),
        Err(why) => common::fail("avx", &why),
    }
}
