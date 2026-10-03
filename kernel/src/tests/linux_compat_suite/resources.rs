//! Resource reporting and process attributes: rlimits, `sysinfo`, `times`,
//! `getrusage`, `prctl`, `madvise` and robust futex lists.

use super::*;

const GETRLIMIT: u64 = 97;
const SETRLIMIT: u64 = 160;
const PRLIMIT64: u64 = 302;
const SYSINFO: u64 = 99;
const TIMES: u64 = 100;
const GETRUSAGE: u64 = 98;
const PRCTL: u64 = 157;
const MADVISE: u64 = 28;
const SET_ROBUST_LIST: u64 = 273;
const GET_ROBUST_LIST: u64 = 274;
const RLIMIT_STACK: u64 = 3;
const RLIMIT_CORE: u64 = 4;
const RLIMIT_NOFILE: u64 = 7;

fn getrlimit(resource: u64) -> Result<[u64; 2], String> {
    let mut out = [0u64; 2];
    let got = sys(GETRLIMIT, &[resource, out.as_mut_ptr() as u64]);
    check!(got == 0, "getrlimit({resource}) = {got:#x}");
    Ok(out)
}

/// The limits are the kernel's real ones; only no-op changes (and any core
/// limit) are accepted.
pub fn rlimits_honest() -> Result<(), String> {
    fresh()?;
    let nofile = getrlimit(RLIMIT_NOFILE)?;
    check!(nofile == [task::fd_max() as u64; 2], "NOFILE {nofile:?}");
    let stack = getrlimit(RLIMIT_STACK)?;
    check!(
        stack == [process::linux::stack_size(); 2],
        "STACK {stack:?}"
    );
    check!(
        sys(SETRLIMIT, &[RLIMIT_NOFILE, nofile.as_ptr() as u64]) == 0,
        "a no-op set failed"
    );
    let lower = [4u64, nofile[1]];
    check!(
        sys(SETRLIMIT, &[RLIMIT_NOFILE, lower.as_ptr() as u64]) == EPERM,
        "an unenforced limit was accepted"
    );
    let inverted = [10u64, 5];
    check!(
        sys(SETRLIMIT, &[RLIMIT_NOFILE, inverted.as_ptr() as u64]) == EINVAL,
        "soft > hard accepted"
    );
    let core = [0u64, 0];
    check!(
        sys(SETRLIMIT, &[RLIMIT_CORE, core.as_ptr() as u64]) == 0,
        "a core limit was refused"
    );
    let mut old = [0u64; 2];
    check!(
        sys(PRLIMIT64, &[0, RLIMIT_NOFILE, 0, old.as_mut_ptr() as u64]) == 0 && old == nofile,
        "prlimit64 read {old:?}"
    );
    check!(
        sys(GETRLIMIT, &[99, old.as_mut_ptr() as u64]) == EINVAL,
        "resource 99 accepted"
    );
    check!(
        sys_checked(GETRLIMIT, &[RLIMIT_NOFILE, 8]) == EFAULT,
        "a bad pointer was not EFAULT"
    );
    Ok(())
}

/// `sysinfo` reports uptime and memory; `times` returns the tick counter;
/// `getrusage` accepts SELF/CHILDREN/THREAD only.
pub fn sysinfo_times_rusage() -> Result<(), String> {
    fresh()?;
    let mut info = [0u8; 112];
    check!(sys(SYSINFO, &[info.as_mut_ptr() as u64]) == 0, "sysinfo");
    let word = |at: usize| u64::from_le_bytes(info[at..at + 8].try_into().unwrap());
    let stats = crate::mem::frame_stats();
    check!(
        word(32) == stats.total as u64,
        "totalram {} vs {}",
        word(32),
        stats.total
    );
    check!(word(40) <= word(32), "freeram above totalram");
    check!(
        u32::from_le_bytes(info[104..108].try_into().unwrap()) == 4096,
        "mem_unit"
    );
    check!(word(0) == task::ticks() / 100, "uptime {}", word(0));
    let mut tms = [u64::MAX; 4];
    let now = sys(TIMES, &[tms.as_mut_ptr() as u64]);
    check!(now == task::ticks(), "times() = {now}");
    check!(tms[1] == 0 && tms[2] == 0 && tms[3] == 0, "tms {tms:?}");
    let mut usage = [0xffu8; 144];
    for who in [0u64, u64::MAX, 1] {
        check!(
            sys(GETRUSAGE, &[who, usage.as_mut_ptr() as u64]) == 0,
            "getrusage({who:#x})"
        );
    }
    check!(
        sys(GETRUSAGE, &[5, usage.as_mut_ptr() as u64]) == EINVAL,
        "who 5 accepted"
    );
    Ok(())
}

/// `prctl` names round-trip and unknown options are refused; `madvise`
/// `DONTNEED` zeroes anonymous memory and refuses what it cannot honour.
pub fn prctl_and_madvise() -> Result<(), String> {
    fresh()?;
    let name = cpath("compat-worker-thread");
    check!(sys(PRCTL, &[15, name.as_ptr() as u64]) == 0, "PR_SET_NAME");
    let mut got = [0xffu8; 16];
    check!(
        sys(PRCTL, &[16, got.as_mut_ptr() as u64]) == 0,
        "PR_GET_NAME"
    );
    check!(
        &got[..15] == b"compat-worker-t" && got[15] == 0,
        "name {:?}",
        &got
    );
    check!(
        sys(PRCTL, &[38, 1, 0]) == 0 && sys(PRCTL, &[39]) == 1,
        "NO_NEW_PRIVS"
    );
    check!(
        sys(PRCTL, &[1, 9]) == EINVAL,
        "a parent-death signal was accepted"
    );
    check!(sys(PRCTL, &[1, 0]) == 0, "PDEATHSIG 0 refused");
    check!(
        sys(PRCTL, &[0x1234]) == EINVAL,
        "an unknown option was accepted"
    );
    check!(sys(PRCTL, &[4, 2]) == EINVAL, "dumpable 2 accepted");
    // madvise DONTNEED on anonymous memory reads back as zeros.
    let base = 0x5100_0000u64;
    let mapped = sys(9, &[base, 2 * 4096, 3, 0x22 | 0x10, u64::MAX, 0]);
    check!(mapped == base, "mmap {mapped:#x}");
    // SAFETY: the two pages at `base` were just mapped read-write above.
    unsafe { core::ptr::write_bytes(base as *mut u8, 0xAB, 8192) };
    check!(sys(MADVISE, &[base, 8192, 4]) == 0, "DONTNEED");
    // SAFETY: still mapped; DONTNEED keeps the mapping.
    let back = unsafe { core::ptr::read_volatile((base + 4100) as *const u8) };
    check!(back == 0, "DONTNEED left {back:#x}");
    check!(
        sys(MADVISE, &[base, 8192, 0]) == 0 && sys(MADVISE, &[base, 8192, 14]) == 0,
        "hints refused"
    );
    check!(
        sys(MADVISE, &[base + 1, 4096, 0]) == EINVAL,
        "an unaligned address was accepted"
    );
    check!(
        sys(MADVISE, &[base, 4096, 18]) == EINVAL,
        "WIPEONFORK accepted"
    );
    check!(
        sys(MADVISE, &[base, 4096, 9]) == EINVAL,
        "MADV_REMOVE accepted"
    );
    sys(11, &[base, 8192]);
    Ok(())
}

/// A thread that exits holding a robust mutex leaves it `OWNER_DIED`; a lock
/// owned by someone else is untouched; a bad length is refused.
pub fn robust_list_owner_died() -> Result<(), String> {
    fresh()?;
    // struct robust_list_head { next, futex_offset, list_op_pending } and two
    // entries whose lock words sit 8 bytes after their list node.
    let mut memory = [0u64; 8];
    let base = memory.as_mut_ptr() as u64;
    let (head, first, second) = (base, base + 24, base + 40);
    let me = task::current() as u64;
    let words = [
        first,
        8,
        0,
        second,           // first.next
        me | 0x8000_0000, // first's word: ours, with waiters
        head,             // second.next: back to the head
        4242,             // second's word: someone else's
    ];
    for (index, word) in words.into_iter().enumerate() {
        // SAFETY: `base` points at `memory`, eight live words.
        unsafe { (base as *mut u64).add(index).write_volatile(word) };
    }
    check!(
        sys(SET_ROBUST_LIST, &[head, 16]) == EINVAL,
        "a bad length was accepted"
    );
    check!(sys(SET_ROBUST_LIST, &[head, 24]) == 0, "set_robust_list");
    let (mut got_head, mut got_len) = (0u64, 0u64);
    check!(
        sys(
            GET_ROBUST_LIST,
            &[
                0,
                &mut got_head as *mut u64 as u64,
                &mut got_len as *mut u64 as u64
            ]
        ) == 0,
        "get"
    );
    check!(
        got_head == head && got_len == 24,
        "get_robust_list {got_head:#x}/{got_len}"
    );
    process::linux::robust_exit_for_test(task::current());
    // SAFETY: `memory` is live; the kernel wrote it through a raw pointer.
    let (word1, word2) = unsafe {
        (
            core::ptr::read_volatile((first + 8) as *const u64) as u32,
            core::ptr::read_volatile((second + 8) as *const u64) as u32,
        )
    };
    check!(
        word1 == 0x8000_0000 | 0x4000_0000,
        "our lock word is {word1:#x}"
    );
    check!(word2 == 4242, "a foreign lock word changed to {word2:#x}");
    Ok(())
}

/// Thousands of mixed resource calls keep answering the same.
pub fn resources_soak() -> Result<(), String> {
    fresh()?;
    let want = getrlimit(RLIMIT_NOFILE)?;
    let mut info = [0u8; 112];
    for round in 0..5000u64 {
        let mut out = [0u64; 2];
        check!(
            sys(PRLIMIT64, &[0, round % 16, 0, out.as_mut_ptr() as u64]) == 0,
            "round {round}"
        );
        if round % 16 == RLIMIT_NOFILE {
            check!(out == want, "round {round}: NOFILE changed");
        }
        check!(
            sys(SYSINFO, &[info.as_mut_ptr() as u64]) == 0,
            "round {round}: sysinfo"
        );
        let mut name = [0u8; 16];
        check!(
            sys(PRCTL, &[16, name.as_mut_ptr() as u64]) == 0,
            "round {round}: name"
        );
    }
    Ok(())
}
