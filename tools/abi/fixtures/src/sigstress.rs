//! `sigstress` — raw signals: `rt_sigaction`, `rt_sigprocmask`, `kill` and a
//! `panic::catch_unwind` round-trip.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SYS_RT_SIGACTION: u64 = 13;
const SYS_RT_SIGPROCMASK: u64 = 14;
const SYS_KILL: u64 = 62;

const SIGUSR1: usize = 10;
const SIGUSR2: usize = 12;
const SIG_BLOCK: usize = 0;
const SIG_UNBLOCK: usize = 2;

/// x86_64 `SA_RESTORER`: the handler's return goes through `sa_restorer`.
const SA_RESTORER: u64 = 0x0400_0000;

static USR1_RUNS: AtomicUsize = AtomicUsize::new(0);
static USR2_RUNS: AtomicUsize = AtomicUsize::new(0);
static USR1_BLOCKED_IN_HANDLER: AtomicBool = AtomicBool::new(false);

// The kernel's `struct sigaction` layout is `{ handler, flags, restorer, mask }`
// (32 bytes). The handler returns into this stub, which issues `rt_sigreturn`.
core::arch::global_asm!(
    r#"
    .globl __abi_restore_rt
    __abi_restore_rt:
        mov rax, 15
        syscall
"#
);

unsafe extern "C" {
    fn __abi_restore_rt();
}

#[repr(C)]
struct KernelSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

unsafe fn syscall2(nr: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") a1,
        in("rsi") a2,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

unsafe fn syscall4(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

fn install(sig: usize, handler: extern "C" fn(i32)) -> i64 {
    let action = KernelSigaction {
        handler: handler as usize,
        flags: SA_RESTORER,
        restorer: __abi_restore_rt as *const () as usize,
        mask: 0,
    };
    unsafe {
        syscall4(
            SYS_RT_SIGACTION,
            sig as u64,
            &action as *const _ as u64,
            0,
            8,
        )
    }
}

extern "C" fn usr1_handler(_sig: i32) {
    USR1_RUNS.fetch_add(1, Ordering::SeqCst);
    // The signal must be blocked while its own handler runs.
    let mut mask = 0u64;
    unsafe {
        syscall4(
            SYS_RT_SIGPROCMASK,
            SIG_BLOCK as u64,
            0,
            &mut mask as *mut u64 as u64,
            8,
        )
    };
    if mask & (1 << (SIGUSR1 - 1)) != 0 {
        USR1_BLOCKED_IN_HANDLER.store(true, Ordering::SeqCst);
    }
}

extern "C" fn usr2_handler(_sig: i32) {
    USR2_RUNS.fetch_add(1, Ordering::SeqCst);
}

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

fn main() {
    let mut first: Option<String> = None;

    if install(SIGUSR1, usr1_handler) != 0 {
        note(&mut first, "rt_sigaction SIGUSR1 failed".to_string());
    }
    if install(SIGUSR2, usr2_handler) != 0 {
        note(&mut first, "rt_sigaction SIGUSR2 failed".to_string());
    }

    // A self-signal runs the handler on the way out of the `kill` syscall.
    if unsafe { syscall2(SYS_KILL, 0, SIGUSR1 as u64) } != 0 {
        note(&mut first, "kill(SIGUSR1) failed".to_string());
    }
    if USR1_RUNS.load(Ordering::SeqCst) != 1 {
        note(
            &mut first,
            format!(
                "SIGUSR1 handler ran {} times",
                USR1_RUNS.load(Ordering::SeqCst)
            ),
        );
    }
    if !USR1_BLOCKED_IN_HANDLER.load(Ordering::SeqCst) {
        note(
            &mut first,
            "signal was not blocked inside its handler".to_string(),
        );
    }

    // A blocked signal stays pending and is delivered when unblocked.
    let usr2 = 1u64 << (SIGUSR2 - 1);
    if unsafe {
        syscall4(
            SYS_RT_SIGPROCMASK,
            SIG_BLOCK as u64,
            &usr2 as *const u64 as u64,
            0,
            8,
        )
    } != 0
    {
        note(&mut first, "sigprocmask block failed".to_string());
    }
    if unsafe { syscall2(SYS_KILL, 0, SIGUSR2 as u64) } != 0 {
        note(&mut first, "kill(SIGUSR2) failed".to_string());
    }
    if USR2_RUNS.load(Ordering::SeqCst) != 0 {
        note(&mut first, "blocked SIGUSR2 handler ran".to_string());
    }
    if unsafe {
        syscall4(
            SYS_RT_SIGPROCMASK,
            SIG_UNBLOCK as u64,
            &usr2 as *const u64 as u64,
            0,
            8,
        )
    } != 0
    {
        note(&mut first, "sigprocmask unblock failed".to_string());
    }
    if USR2_RUNS.load(Ordering::SeqCst) != 1 {
        note(
            &mut first,
            "pending SIGUSR2 was not delivered on unblock".to_string(),
        );
    }

    // `catch_unwind` must return `Err` without aborting the process.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_info| {}));
    let caught = std::panic::catch_unwind(|| panic!("abi sigstress panic")).is_err();
    std::panic::set_hook(previous);
    if !caught {
        note(
            &mut first,
            "catch_unwind did not catch the panic".to_string(),
        );
    }

    match first {
        Some(reason) => common::fail("sigstress", &reason),
        None => common::pass("sigstress"),
    }
}
