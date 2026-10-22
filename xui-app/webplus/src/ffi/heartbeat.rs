//! The UI process's heartbeat (issue #861): a thread that speaks while the others are stuck. When a
//! freeze happens the pictures stop and the markers with them, which says nothing about *who* is
//! stuck; this thread keeps a clock of its own and asks each of the two threads that matter whether
//! they still turn.
//!
//! * WebKit's main thread, which runs the page's IPC, the paint and the window's calls to the shell:
//!   a ping is posted to it once a second. `INFO:MAINLAG:<ms>` when the ping ran more than 250 ms
//!   late, `STALL:main:<ms>` from this thread when it has not run after 2 s (again every 5 s while it
//!   stays stuck).
//! * The window's loop (`xui_app::stall::pass_age_ms`): `STALL:window:<ms>` when one pass has
//!   lasted 2 s, every 5 s while it does.
//! * `INFO:CPU:ui=<pct>:web=<pct>:net=<pct>` every 5 s when any is 20% or more (`xui_webkit::taskstat`).
//! * `INFO:REAPER:wakeups=<n>` at 10 s, at 30 s and whenever the launcher's reaper threads have woken
//!   since (issue #887): one wake per engine process that ends, so a count that keeps growing in an
//!   idle browser is a thread that polls.
//!
//! The thread starts with the window and ends with it (`stop`, before the run loop is left): it times
//! the main thread only once its loop has begun to turn (`lzw_engine_running`), and every line that can
//! repeat has a budget (`xui_app::trace::within`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use xui_app::trace::{self, Budget, Channel};
use xui_webkit::taskstat;

use super::*;

extern "C" {
    /// `lazyos_ui.h`: the ids of the page's WebProcess and of the NetworkProcess (0: none yet).
    fn lzw_engine_children(web: *mut c_int, network: *mut c_int);
    /// `lazyos_ui.h`: whether WebKit's run loop has begun to turn.
    fn lzw_engine_running() -> c_int;
    /// `lazyos_ui.h`: how many times the launcher's reaper threads have come back from waiting.
    fn lzw_launcher_reaper_wakeups() -> std::ffi::c_ulong;
}

/// The window is closing: the heartbeat ends at its next look.
static STOP: AtomicBool = AtomicBool::new(false);

/// The lines that can repeat, each a few per minute at most (the `SUPPRESSED` notice follows).
static MAIN_BUDGET: Budget = Budget::new(6, 6000);
static WINDOW_BUDGET: Budget = Budget::new(6, 6000);
static LAG_BUDGET: Budget = Budget::new(12, 6000);
static CPU_BUDGET: Budget = Budget::new(12, 6000);
static REAPER_BUDGET: Budget = Budget::new(12, 6000);

/// Prints `line` when `budget` has some left.
fn say(budget: &Budget, line: &str) {
    if trace::within(budget, Channel::Webplus) {
        crate::marker(line);
    }
}

const TICK: Duration = Duration::from_secs(1);
/// A ping that ran this late is worth a line.
const LAG_REPORT: Duration = Duration::from_millis(250);
/// A thread that has not turned for this long is stuck.
const STUCK: Duration = Duration::from_secs(2);
/// A stuck thread is reported this often.
const REPEAT: Duration = Duration::from_secs(5);
/// The CPU line is looked at every this many ticks.
const CPU_EVERY: u32 = 5;
/// Ticks (seconds) at which the launcher's wake count is always printed; later only when it changed.
const REAPER_FIRST: u32 = 10;
const REAPER_SECOND: u32 = 30;
/// A process below this share of the CPU is not worth a line.
const CPU_MIN_PERCENT: u64 = 20;

/// What the main thread's ping writes: the number of the last ping it ran and how late it was.
#[derive(Default)]
struct Pong {
    answered: AtomicU64,
    lag_ms: AtomicU64,
}

/// Starts the heartbeat thread (one per process).
pub(super) fn start() {
    let _ = std::thread::Builder::new()
        .name(String::from("heartbeat"))
        .spawn(run);
}

/// Ends the heartbeat (the window is closing; the run loop is about to be left, and a ping to it would
/// never be answered).
pub(super) fn stop() {
    STOP.store(true, Ordering::Release);
}

/// A rate limit for a line that repeats while a condition lasts.
struct Every(Option<Instant>);

impl Every {
    /// True when the line may be printed now (and counts it as printed).
    fn due(&mut self, gap: Duration) -> bool {
        if self.0.is_some_and(|at| at.elapsed() < gap) {
            return false;
        }
        self.0 = Some(Instant::now());
        true
    }
}

fn run() {
    crate::marker("HEARTBEAT:UP");
    let pong = Arc::new(Pong::default());
    let mut sent = 0u64;
    let mut outstanding: Option<(u64, Instant)> = None;
    let (mut main_line, mut window_line) = (Every(None), Every(None));
    let mut cpu = Cpu::default();
    let mut ticks = 0u32;
    let mut reaper_said = None::<u64>;
    loop {
        std::thread::sleep(TICK);
        if STOP.load(Ordering::Acquire) {
            return;
        }
        if let Some((number, posted)) = outstanding {
            if pong.answered.load(Ordering::Acquire) >= number {
                let lag = pong.lag_ms.load(Ordering::Acquire);
                if Duration::from_millis(lag) >= LAG_REPORT {
                    say(&LAG_BUDGET, &format!("INFO:MAINLAG:{lag}"));
                }
                outstanding = None;
                main_line = Every(None);
            } else if posted.elapsed() >= STUCK && main_line.due(REPEAT) {
                say(
                    &MAIN_BUDGET,
                    &format!("STALL:main:{}", posted.elapsed().as_millis()),
                );
            }
        }
        // SAFETY: no arguments.
        let turning = unsafe { lzw_engine_running() } != 0;
        if outstanding.is_none() && turning {
            sent += 1;
            let (number, posted, pong) = (sent, Instant::now(), Arc::clone(&pong));
            on_main_detached(move || {
                pong.lag_ms
                    .store(posted.elapsed().as_millis() as u64, Ordering::Release);
                pong.answered.store(number, Ordering::Release);
            });
            outstanding = Some((number, posted));
        }
        match xui_app::stall::pass_age_ms() {
            Some(age) if Duration::from_millis(age) >= STUCK => {
                if window_line.due(REPEAT) {
                    say(&WINDOW_BUDGET, &format!("STALL:window:{age}"));
                }
            }
            _ => window_line = Every(None),
        }
        ticks += 1;
        if ticks % CPU_EVERY == 0 {
            cpu.report();
        }
        // SAFETY: no arguments; the counter is an atomic.
        let wakeups = u64::from(unsafe { lzw_launcher_reaper_wakeups() });
        if ticks == REAPER_FIRST || ticks == REAPER_SECOND || reaper_said.is_some_and(|said| said != wakeups) {
            say(&REAPER_BUDGET, &format!("INFO:REAPER:wakeups={wakeups}"));
        }
        if ticks >= REAPER_FIRST {
            reaper_said = Some(wakeups);
        }
    }
}

/// CPU use between two looks at the task list.
#[derive(Default)]
struct Cpu {
    last: Option<(Instant, [u64; 3])>,
    said_unavailable: bool,
}

impl Cpu {
    fn report(&mut self) {
        let Some(rows) = taskstat::snapshot() else {
            if !self.said_unavailable {
                self.said_unavailable = true;
                crate::marker("INFO:CPU:UNAVAILABLE");
            }
            return;
        };
        let (mut web, mut network): (c_int, c_int) = (0, 0);
        // SAFETY: both pointers are valid for a write; the engine only stores the two ids.
        unsafe { lzw_engine_children(&mut web, &mut network) };
        let own = taskstat::group_of(&rows, u64::from(std::process::id()));
        let groups = [own, u64::try_from(web).ok(), u64::try_from(network).ok()];
        let used = groups.map(|group| match group {
            // A child that shares the window's group (its own setpgid failed) is counted once.
            Some(g) if g != 0 => taskstat::group_ticks(&rows, g),
            _ => 0,
        });
        let now = Instant::now();
        if self.last.is_none() {
            // Once: the groups the shares are summed over, so a line of zeros can be told from a
            // task list this cannot read.
            crate::marker(&format!(
                "INFO:CPU:GROUPS:ui={}:web={web}:net={network}:threads={}",
                own.map_or(-1, |g| g as i64),
                rows.len()
            ));
        }
        if let Some((before, last)) = self.last.replace((now, used)) {
            let elapsed = now.duration_since(before);
            let share =
                [0, 1, 2].map(|at| taskstat::percent(used[at].saturating_sub(last[at]), elapsed));
            if share.iter().any(|&p| p >= CPU_MIN_PERCENT) {
                say(
                    &CPU_BUDGET,
                    &format!("INFO:CPU:ui={}:web={}:net={}", share[0], share[1], share[2]),
                );
            }
        }
    }
}
