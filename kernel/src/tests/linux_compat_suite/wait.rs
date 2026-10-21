//! `wait4`/`waitid`: the pid argument selects the child, a signal death is
//! `WIFSIGNALED`, `ECHILD`/`WNOHANG` behave as on Linux, and `waitid(WNOWAIT)`
//! reports a child without collecting it (the launcher's reaper threads, #887).

use super::*;

const WAIT4: u64 = 61;
const WAITID: u64 = 247;
const WNOHANG: u64 = 1;

/// A forked child of the current task.
fn child() -> Result<usize, String> {
    task::spawn_fork().map_err(|error| format!("fork: {error}"))
}

fn wait4(pid: i64, status: &mut u32, options: u64) -> u64 {
    sys(WAIT4, &[pid as u64, status as *mut u32 as u64, options, 0])
}

/// A leader process to fork from (forking from the kernel task gives each
/// child a fresh group of its own).
fn leader() -> Result<usize, String> {
    let leader = child()?;
    task::harness::switch_current(leader);
    Ok(leader)
}

/// A positive pid reaps exactly that child; `-1` any; `0` and `-pgid` the
/// group; a non-child pid is `ECHILD`; `WNOHANG` returns 0 while it runs.
pub fn wait4_pid_selection() -> Result<(), String> {
    fresh()?;
    let parent = leader()?;
    let (a, b) = (child()?, child()?);
    let mut status = 0u32;
    check!(
        wait4(a as i64, &mut status, WNOHANG) == 0,
        "WNOHANG reaped a running child"
    );
    task::harness::finish(a, 3);
    task::harness::finish(b, 4);
    let got = wait4(b as i64, &mut status, 0);
    check!(got == b as u64, "wait4({b}) reaped {got}");
    check!(status == 4 << 8, "status {status:#x}, want exit 4");
    check!(
        wait4(b as i64, &mut status, WNOHANG) == ECHILD,
        "a reaped pid is still a child"
    );
    check!(
        wait4(9999, &mut status, 0) == ECHILD,
        "an unknown pid did not ECHILD"
    );
    let got = wait4(-1, &mut status, 0);
    check!(
        got == a as u64 && status == 3 << 8,
        "wait4(-1) gave {got}/{status:#x}"
    );
    check!(
        wait4(-1, &mut status, 0) == ECHILD,
        "no children left, but no ECHILD"
    );
    // Process groups: a child in the caller's group, one in a group of its own.
    let (same, other) = (child()?, child()?);
    check!(
        task::process::setpgid(parent, other as i64, other as i64).is_ok(),
        "setpgid"
    );
    task::harness::finish(same, 5);
    task::harness::finish(other, 6);
    let got = wait4(-(other as i64), &mut status, 0);
    check!(
        got == other as u64,
        "wait4(-pgid) reaped {got}, want {other}"
    );
    let got = wait4(0, &mut status, 0);
    check!(got == same as u64, "wait4(0) reaped {got}, want {same}");
    check!(
        sys(WAIT4, &[u64::MAX, 0, 0x8000_0000_0000, 0]) == EINVAL,
        "bad options accepted"
    );
    task::harness::reset();
    Ok(())
}

/// A child killed by a signal reports `WIFSIGNALED` with that signal (and
/// the core bit for `SIGSEGV`); a plain `exit(137)` stays an exit.
pub fn wait4_signal_status() -> Result<(), String> {
    fresh()?;
    leader()?;
    let killed = child()?;
    let pml4 = task::harness::pml4(killed).ok_or("child has no table")?;
    task::signal::terminate_process(pml4, 128 + 9);
    let mut status = 0u32;
    check!(
        wait4(killed as i64, &mut status, 0) == killed as u64,
        "reap killed"
    );
    check!(status == 9, "SIGKILL status {status:#x}, want 9");
    let faulted = child()?;
    let pml4 = task::harness::pml4(faulted).ok_or("child has no table")?;
    task::signal::terminate_process(pml4, 128 + 11);
    check!(
        wait4(faulted as i64, &mut status, 0) == faulted as u64,
        "reap faulted"
    );
    check!(
        status == 0x80 | 11,
        "SIGSEGV status {status:#x}, want core + 11"
    );
    let exited = child()?;
    task::harness::finish(exited, 137);
    check!(
        wait4(exited as i64, &mut status, 0) == exited as u64,
        "reap exited"
    );
    check!(status == 137 << 8, "exit(137) status {status:#x}");
    task::harness::reset();
    Ok(())
}

/// `waitid(P_PID)` fills `siginfo_t` with `CLD_EXITED` and the code;
/// `WEXITED` is required.
pub fn waitid_reports_exit() -> Result<(), String> {
    fresh()?;
    leader()?;
    let kid = child()?;
    let mut info = [0u8; 128];
    let ptr = info.as_mut_ptr() as u64;
    check!(
        sys(WAITID, &[1, kid as u64, ptr, 0, 0]) == EINVAL,
        "no WEXITED accepted"
    );
    task::harness::finish(kid, 7);
    check!(
        sys(WAITID, &[1, kid as u64, ptr, 4, 0]) == 0,
        "waitid failed"
    );
    let word = |at: usize| i32::from_le_bytes(info[at..at + 4].try_into().unwrap());
    check!(
        word(0) == 17 && word(8) == 1,
        "signo {} code {}",
        word(0),
        word(8)
    );
    check!(
        word(16) == kid as i32 && word(24) == 7,
        "pid {} status {}",
        word(16),
        word(24)
    );
    task::harness::reset();
    Ok(())
}

/// Many fork/exit/reap rounds in shuffled order: each `wait4(pid)` collects
/// exactly its child with its status, and no slot is left behind.
pub fn wait4_soak() -> Result<(), String> {
    fresh()?;
    let parent = leader()?;
    let free_before = task::free_slots();
    for round in 0..300usize {
        let kids: Vec<usize> = (0..4).map(|_| child()).collect::<Result<_, _>>()?;
        for (i, &kid) in kids.iter().enumerate() {
            task::harness::finish(kid, ((round + i) & 0x7f) as u64);
        }
        for i in [2usize, 0, 3, 1] {
            let mut status = 0u32;
            let got = wait4(kids[i] as i64, &mut status, 0);
            check!(
                got == kids[i] as u64,
                "round {round}: reaped {got}, want {}",
                kids[i]
            );
            let want = (((round + i) & 0x7f) as u32) << 8;
            check!(
                status == want,
                "round {round}: status {status:#x}, want {want:#x}"
            );
        }
        task::harness::switch_current(parent);
    }
    check!(
        task::free_slots() == free_before,
        "slots leaked: {} -> {}",
        free_before,
        task::free_slots()
    );
    task::harness::reset();
    Ok(())
}

// `waitid(WNOWAIT)` (issue #887): report a finished child and leave it waitable.

const P_ALL: u64 = 0;
const P_PID: u64 = 1;
const WEXITED: u64 = 4;
const WNOWAIT: u64 = 0x0100_0000;
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;

/// The words of a `siginfo_t` that `waitid` fills: `si_signo`, `si_code`,
/// `si_pid` and `si_status`.
struct Info([u8; 128]);

impl Info {
    fn new() -> Self {
        Info([0; 128])
    }

    fn word(&self, at: usize) -> i32 {
        i32::from_le_bytes(self.0[at..at + 4].try_into().unwrap_or([0; 4]))
    }

    fn signo(&self) -> i32 {
        self.word(0)
    }

    fn code(&self) -> i32 {
        self.word(8)
    }

    fn pid(&self) -> i32 {
        self.word(16)
    }

    fn status(&self) -> i32 {
        self.word(24)
    }
}

/// `waitid(idtype, id, &info, options)`.
fn waitid(idtype: u64, id: usize, options: u64, info: &mut Info) -> u64 {
    sys(WAITID, &[idtype, id as u64, info.0.as_mut_ptr() as u64, options, 0])
}

fn reap(pid: usize) -> (u64, u32) {
    let mut status = 0u32;
    let got = sys(WAIT4, &[pid as u64, &mut status as *mut u32 as u64, 0, 0]);
    (got, status)
}

/// A running child is not reported (`WNOHANG`), a finished one is, as often as
/// asked, and stays a child until a plain wait collects it; a signal death
/// reports `CLD_KILLED` with the signal and also stays.
pub fn waitid_nowait_keeps_child() -> Result<(), String> {
    fresh()?;
    let parent = child()?;
    task::harness::switch_current(parent);
    let kid = child()?;
    let mut info = Info::new();
    check!(
        waitid(P_PID, kid, WEXITED | WNOHANG | WNOWAIT, &mut info) == 0,
        "WNOHANG|WNOWAIT on a running child failed"
    );
    check!(info.pid() == 0 && info.signo() == 0, "a running child was reported");
    check!(task::is_child(kid), "a running child stopped being a child");

    task::harness::finish(kid, 7);
    for round in 0..3 {
        let mut info = Info::new();
        check!(
            waitid(P_PID, kid, WEXITED | WNOWAIT, &mut info) == 0,
            "WNOWAIT of a finished child failed (round {round})"
        );
        check!(
            info.pid() == kid as i32 && info.code() == CLD_EXITED && info.status() == 7,
            "round {round}: pid {} code {} status {}",
            info.pid(),
            info.code(),
            info.status()
        );
        check!(task::is_child(kid), "WNOWAIT collected the child (round {round})");
    }
    // P_ALL sees it too, and the plain wait still gets the real status.
    let mut info = Info::new();
    check!(
        waitid(P_ALL, 0, WEXITED | WNOWAIT, &mut info) == 0 && info.pid() == kid as i32,
        "P_ALL|WNOWAIT did not report the child"
    );
    let (got, status) = reap(kid);
    check!(got == kid as u64, "wait4 reaped {got}, want {kid}");
    check!(status == 7 << 8, "wait4 status {status:#x}, want exit 7");
    check!(
        waitid(P_PID, kid, WEXITED | WNOWAIT, &mut info) == ECHILD,
        "a collected child is still waitable"
    );
    check!(!task::is_child(kid), "the collected child is still a child");

    // A child killed by a signal: CLD_KILLED, the signal as status, still there.
    let killed = child()?;
    let pml4 = task::harness::pml4(killed).ok_or("child has no table")?;
    task::signal::terminate_process(pml4, 128 + 9);
    let mut info = Info::new();
    check!(
        waitid(P_PID, killed, WEXITED | WNOWAIT, &mut info) == 0,
        "WNOWAIT of a killed child failed"
    );
    check!(
        info.code() == CLD_KILLED && info.status() == 9,
        "code {} status {}, want killed by 9",
        info.code(),
        info.status()
    );
    check!(task::is_child(killed), "WNOWAIT collected the killed child");
    check!(reap(killed).0 == killed as u64, "wait4 did not collect the killed child");
    task::harness::reset();
    Ok(())
}

/// A pid that is not a child (never was, or was collected) is `ECHILD` with
/// `WNOWAIT` as without it, so a reaper whose child a sibling thread collected
/// is told, not parked.
pub fn waitid_nowait_no_child() -> Result<(), String> {
    fresh()?;
    let parent = child()?;
    task::harness::switch_current(parent);
    let mut info = Info::new();
    check!(
        waitid(P_PID, 9999, WEXITED | WNOWAIT, &mut info) == ECHILD,
        "an unknown pid did not ECHILD"
    );
    let kid = child()?;
    task::harness::finish(kid, 1);
    check!(reap(kid).0 == kid as u64, "wait4 did not reap");
    check!(
        waitid(P_PID, kid, WEXITED | WNOWAIT, &mut info) == ECHILD,
        "a reaped pid did not ECHILD"
    );
    check!(
        waitid(P_ALL, 0, WEXITED | WNOWAIT, &mut info) == ECHILD,
        "no children, but no ECHILD"
    );
    check!(
        waitid(P_PID, kid, WNOWAIT, &mut info) == EINVAL,
        "WNOWAIT without WEXITED accepted"
    );
    task::harness::reset();
    Ok(())
}

/// Many rounds of fork, exit, peek in shuffled order, reap: a peek never
/// consumes a child, every reap still gets its own status, and no slot is left
/// behind.
pub fn waitid_nowait_soak() -> Result<(), String> {
    fresh()?;
    let parent = child()?;
    task::harness::switch_current(parent);
    let free_before = task::free_slots();
    for round in 0..300usize {
        let kids: Vec<usize> = (0..4).map(|_| child()).collect::<Result<_, _>>()?;
        for (i, &kid) in kids.iter().enumerate() {
            task::harness::finish(kid, ((round + i) & 0x7f) as u64);
        }
        for i in [3usize, 1, 0, 2, 1] {
            let mut info = Info::new();
            check!(
                waitid(P_PID, kids[i], WEXITED | WNOWAIT, &mut info) == 0,
                "round {round}: peek of {} failed",
                kids[i]
            );
            let want = ((round + i) & 0x7f) as i32;
            check!(
                info.pid() == kids[i] as i32 && info.status() == want,
                "round {round}: peek gave pid {} status {}, want {} / {want}",
                info.pid(),
                info.status(),
                kids[i]
            );
        }
        for i in [2usize, 0, 3, 1] {
            let (got, status) = reap(kids[i]);
            let want = (((round + i) & 0x7f) as u32) << 8;
            check!(
                got == kids[i] as u64 && status == want,
                "round {round}: reaped {got}/{status:#x}, want {}/{want:#x}",
                kids[i]
            );
        }
    }
    check!(
        task::free_slots() == free_before,
        "slots leaked: {} -> {}",
        free_before,
        task::free_slots()
    );
    task::harness::reset();
    Ok(())
}
