//! Signal numbers, `sa_flags`, `sigprocmask` how-values and `si_code` constants (x86_64 Linux).

// Signal numbers (x86_64 Linux). The table is complete on purpose: the
// dispatcher must classify any number a Linux binary sends, even if no LazyOS
// code names that signal yet.
#[allow(dead_code)]
pub const SIGHUP: u8 = 1;
pub const SIGINT: u8 = 2;
pub const SIGQUIT: u8 = 3;
pub const SIGILL: u8 = 4;
pub const SIGTRAP: u8 = 5;
pub const SIGABRT: u8 = 6;
pub const SIGBUS: u8 = 7;
pub const SIGFPE: u8 = 8;
pub const SIGKILL: u8 = 9;
#[allow(dead_code)]
pub const SIGUSR1: u8 = 10;
pub const SIGSEGV: u8 = 11;
#[allow(dead_code)]
pub const SIGUSR2: u8 = 12;
#[allow(dead_code)]
pub const SIGPIPE: u8 = 13;
#[allow(dead_code)]
pub const SIGALRM: u8 = 14;
#[allow(dead_code)]
pub const SIGTERM: u8 = 15;
#[allow(dead_code)]
pub const SIGSTKFLT: u8 = 16;
pub const SIGCHLD: u8 = 17;
pub const SIGCONT: u8 = 18;
pub const SIGSTOP: u8 = 19;
pub const SIGTSTP: u8 = 20;
pub const SIGTTIN: u8 = 21;
pub const SIGTTOU: u8 = 22;
pub const SIGURG: u8 = 23;
pub const SIGXCPU: u8 = 24;
pub const SIGXFSZ: u8 = 25;
#[allow(dead_code)]
pub const SIGVTALRM: u8 = 26;
#[allow(dead_code)]
pub const SIGPROF: u8 = 27;
pub const SIGWINCH: u8 = 28;
#[allow(dead_code)]
pub const SIGIO: u8 = 29;
#[allow(dead_code)]
pub const SIGPWR: u8 = 30;
pub const SIGSYS: u8 = 31;

/// Highest supported signal number plus one (`_NSIG`).
pub const NSIG: usize = 65;

// `rt_sigaction` handler sentinels and flags. `SA_RESTORER`/`SA_RESTART` are
// accepted from user space and currently informational.
pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;
pub const SA_SIGINFO: u64 = 0x0000_0004;
#[allow(dead_code)]
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_ONSTACK: u64 = 0x0800_0000;
#[allow(dead_code)]
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

// `rt_sigprocmask` selectors.
pub const SIG_BLOCK: u64 = 0;
pub const SIG_UNBLOCK: u64 = 1;
pub const SIG_SETMASK: u64 = 2;

// `sigaltstack` flags.
pub const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;
/// Minimum alternate stack size Linux accepts (`MINSIGSTKSZ` on x86_64).
pub const MINSIGSTKSZ: u64 = 2048;

// `siginfo.si_code` values used by the kernel.
pub const SI_USER: i32 = 0;
pub const SI_TKILL: i32 = -6;
pub const SI_KERNEL: i32 = 0x80;
pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;
/// `si_code` of a #UD: illegal opcode.
pub const ILL_ILLOPC: i32 = 1;
/// `si_code` of a #DE: integer divide by zero.
pub const FPE_INTDIV: i32 = 1;
