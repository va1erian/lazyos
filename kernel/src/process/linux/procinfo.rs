//! The fabricated `/proc` files that describe the machine and the calling
//! process, beside the mount tables of [`super::procfs`]:
//!
//! * `/proc/cpuinfo` (one processor, from `cpuid`), `/proc/meminfo` (the frame
//!   allocator's totals), `/proc/uptime`, `/proc/loadavg`, `/proc/stat` (one
//!   `cpu` line from the scheduler's idle accounting), `/proc/version`,
//!   `/proc/filesystems`;
//! * `/proc/self/stat`, `/proc/self/status` and `/proc/self/comm`, the parts
//!   of a process's description runtimes and `top`-style tools read for
//!   themselves.
//!
//! Every value is real; fields the kernel does not track are reported as zero.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ipc::credentials;
use crate::task;

/// The paths this module answers.
pub(super) const FILES: &[&str] = &[
    "/proc/cpuinfo",
    "/proc/meminfo",
    "/proc/uptime",
    "/proc/loadavg",
    "/proc/stat",
    "/proc/version",
    "/proc/filesystems",
    "/proc/self/stat",
    "/proc/self/status",
    "/proc/self/comm",
];

/// The `cpuid` brand string, or a generic name on a CPU without one.
fn cpu_brand() -> String {
    let max = core::arch::x86_64::__cpuid(0x8000_0000).eax;
    if max < 0x8000_0004 {
        return String::from("x86_64 processor");
    }
    let mut bytes = Vec::with_capacity(48);
    for leaf in 0x8000_0002..=0x8000_0004u32 {
        let r = core::arch::x86_64::__cpuid(leaf);
        for word in [r.eax, r.ebx, r.ecx, r.edx] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    String::from(text.trim_matches(char::from(0)).trim())
}

/// The `cpuid` vendor string (`GenuineIntel`, `AuthenticAMD`, ...).
fn cpu_vendor() -> String {
    let r = core::arch::x86_64::__cpuid(0);
    let mut bytes = Vec::with_capacity(12);
    for word in [r.ebx, r.edx, r.ecx] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn cpuinfo() -> String {
    let leaf1 = core::arch::x86_64::__cpuid(1);
    let family = (leaf1.eax >> 8) & 0xf;
    let model = (leaf1.eax >> 4) & 0xf;
    let mut flags = String::from("fpu tsc cx8 cmov mmx fxsr sse sse2");
    for (bit, name) in [
        (0, "pni"),
        (9, "ssse3"),
        (19, "sse4_1"),
        (20, "sse4_2"),
        (28, "avx"),
    ] {
        if leaf1.ecx & (1 << bit) != 0 {
            flags.push(' ');
            flags.push_str(name);
        }
    }
    format!(
        "processor\t: 0\nvendor_id\t: {}\ncpu family\t: {family}\nmodel\t\t: {model}\n\
         model name\t: {}\ncpu cores\t: 1\nflags\t\t: {flags}\n\n",
        cpu_vendor(),
        cpu_brand()
    )
}

fn meminfo() -> String {
    let stats = crate::mem::frame_stats();
    let kib = |frames: usize| frames as u64 * 4;
    format!(
        "MemTotal:       {:>8} kB\nMemFree:        {:>8} kB\nMemAvailable:   {:>8} kB\n\
         Buffers:               0 kB\nCached:                0 kB\nSwapTotal:             0 kB\n\
         SwapFree:              0 kB\n",
        kib(stats.total),
        kib(stats.free),
        kib(stats.free)
    )
}

/// Seconds since boot as `s.cc`, the shape `/proc/uptime` uses.
fn seconds(ticks: u64) -> String {
    format!("{}.{:02}", ticks / 100, ticks % 100)
}

fn uptime() -> String {
    format!(
        "{} {}\n",
        seconds(task::ticks()),
        seconds(task::idle_ticks())
    )
}

fn loadavg() -> String {
    let tasks = (1..task::MAX_TASKS)
        .filter(|&slot| task::pml4_of(slot).is_some())
        .count();
    format!("0.00 0.00 0.00 1/{tasks} {}\n", task::current())
}

/// `/proc/stat`: the busy/idle split the scheduler measures, in 100 Hz ticks
/// (Linux's `USER_HZ`).
fn stat() -> String {
    let (all, idle) = (task::ticks(), task::idle_ticks());
    let busy = all.saturating_sub(idle);
    let line = format!("{busy} 0 0 {idle} 0 0 0 0 0 0");
    format!("cpu  {line}\ncpu0 {line}\nbtime 0\nprocesses 0\n")
}

fn version() -> String {
    String::from("LazyOS version 0.1.0 (x86_64) Linux-compatible ABI\n")
}

fn filesystems() -> String {
    String::from("nodev\tramfs\nnodev\tproc\n\text2\n\tvfat\n")
}

/// `/proc/self/stat`: pid, `(comm)`, state, ppid, pgrp, session, and zeros
/// for what is not tracked, with `utime` from the CPU ticks the task used.
fn self_stat() -> String {
    let me = task::current();
    let pid = task::linuxstate::tgid();
    let (comm, len) = task::linuxstate::comm(me);
    let name = String::from_utf8_lossy(&comm[..len]);
    let ppid = task::ppid();
    let pgrp = task::pgid();
    let sid = task::process::sid_of(me);
    let utime = task::cpu_ticks(me);
    let mut fields =
        format!("{pid} ({name}) R {ppid} {pgrp} {sid} 0 -1 0 0 0 0 0 {utime} 0 0 0 20 0 1 0");
    // starttime vsize rss ... up to the 52 fields Linux prints.
    for _ in 22..=52 {
        fields.push_str(" 0");
    }
    fields.push('\n');
    fields
}

fn self_status() -> String {
    let me = task::current();
    let (comm, len) = task::linuxstate::comm(me);
    let cred = credentials::of(me);
    format!(
        "Name:\t{}\nState:\tR (running)\nTgid:\t{}\nPid:\t{me}\nPPid:\t{}\n\
         Uid:\t{u}\t{u}\t{u}\t{u}\nGid:\t{g}\t{g}\t{g}\t{g}\nThreads:\t1\n",
        String::from_utf8_lossy(&comm[..len]),
        task::linuxstate::tgid(),
        task::ppid(),
        u = cred.uid,
        g = cred.gid,
    )
}

fn self_comm() -> String {
    let (comm, len) = task::linuxstate::comm(task::current());
    format!("{}\n", String::from_utf8_lossy(&comm[..len]))
}

/// The bytes of the fabricated file at `path`, if this module answers it.
pub(super) fn contents(path: &str) -> Option<Vec<u8>> {
    let path = path.replace("/proc/thread-self/", "/proc/self/");
    let text = match path.as_str() {
        "/proc/cpuinfo" => cpuinfo(),
        "/proc/meminfo" => meminfo(),
        "/proc/uptime" => uptime(),
        "/proc/loadavg" => loadavg(),
        "/proc/stat" => stat(),
        "/proc/version" => version(),
        "/proc/filesystems" => filesystems(),
        "/proc/self/stat" => self_stat(),
        "/proc/self/status" => self_status(),
        "/proc/self/comm" => self_comm(),
        _ => return None,
    };
    Some(text.into_bytes())
}
