//! Screen blanking (docs/architecture/display.md, "Screen blanking").
//!
//! After `sys/ui/screen/off_minutes` minutes without a key or pointer event the
//! compositor paints the whole screen black and hides the cursor. There is no
//! display driver to switch the monitor off with (the framebuffer is the
//! firmware's or Bochs's), so black is as far as software can go; a monitor
//! that sees a black signal usually sleeps on its own.
//!
//! The first input afterwards wakes the screen and is not delivered: a key or
//! click that only woke the display must not type or press into the window
//! below. A press, key or wheel notch is also dropped for [`GRACE_TICKS`]
//! after a wake: one pointer report can be a move *and* a press, and the move
//! wakes first. Keys `inputd` hands straight to the focused client are held
//! back meanwhile (`NoteKeysHeld`, as for the shell's panel menus), so a person
//! typing into the dark does not type into the app. The kernel's key stream
//! carries PS/2 only, so keys wake the screen through `inputd`'s `KeyActivity`
//! (every keyboard, sent after the hold dropped the key); while `inputd` is
//! attached the kernel stream's keys never wake it, or the hold would end
//! before `inputd` saw that key. The shutting-down overlay wins over the blank
//! screen.
//!
//! The timer is pure (`IdleTimer`) so the boot self-test can drive it. State
//! the renderer needs is one atomic, like `powerfeed`'s.
//!
//! Serial: `XUID:IDLE:BLANK`, `XUID:IDLE:WAKE`, `XUID:IDLE:SET minutes=<n>`.

use core::sync::atomic::{AtomicBool, Ordering};
use user::messenger::display::{Canvas, Color, Rect};
use user::sys;
use user::trace;

use super::compositor::Compositor;
use super::protocol::{Event, EventKind};

/// Ticks (100 Hz) in a minute.
const TICKS_PER_MINUTE: u64 = 6000;
/// Ticks after a wake in which presses, keys and wheel notches are dropped.
const GRACE_TICKS: u64 = 30;

static BLANKED: AtomicBool = AtomicBool::new(false);

/// Whether the screen is blanked (input is held back, the cursor hidden).
pub(super) fn blanked() -> bool {
    BLANKED.load(Ordering::Relaxed)
}

/// Paint the blank screen over `clip`.
pub(super) fn draw(screen: &mut Canvas, clip: Rect) {
    let full = Rect::new(0, 0, screen.width(), screen.height());
    screen.fill(full, clip, Color::rgb(0, 0, 0));
}

/// The idle countdown: when input was last seen and how long it may be quiet.
pub(super) struct IdleTimer {
    last_input: u64,
    /// `0`: never blank.
    minutes: u32,
    /// Presses are dropped until this tick (set by a wake).
    grace_until: u64,
}

impl IdleTimer {
    pub(super) fn new(now: u64) -> IdleTimer {
        IdleTimer {
            last_input: now,
            minutes: 0,
            grace_until: 0,
        }
    }

    /// The screen just woke at `now`: drop presses for a moment.
    pub(super) fn start_grace(&mut self, now: u64) {
        self.grace_until = now + GRACE_TICKS;
    }

    /// Whether presses are still dropped at `now`.
    pub(super) fn in_grace(&self, now: u64) -> bool {
        now < self.grace_until
    }

    /// Input arrived at `now`.
    pub(super) fn note_input(&mut self, now: u64) {
        self.last_input = now;
    }

    /// Use a new delay; a change restarts the countdown (the person who just
    /// picked "1 minute" must not find the screen already dark). `true` when
    /// it changed.
    pub(super) fn set_minutes(&mut self, minutes: u32, now: u64) -> bool {
        if self.minutes == minutes {
            return false;
        }
        self.minutes = minutes;
        self.last_input = now;
        true
    }

    /// Whether the quiet time has run out at `now`.
    pub(super) fn due(&self, now: u64) -> bool {
        self.minutes != 0
            && now.saturating_sub(self.last_input) >= u64::from(self.minutes) * TICKS_PER_MINUTE
    }
}

impl Compositor {
    /// Once per loop pass: follow the setting, and blank when the quiet time
    /// ran out. The shutting-down overlay takes the screen over from a blank.
    pub(super) fn tick_idle(&mut self) {
        let now = sys::clock();
        let minutes = self.themefeed.screen_off_minutes();
        if self.idle.set_minutes(minutes, now) {
            trace::line!(trace::Channel::Xuid, "IDLE:SET minutes={minutes}");
        }
        if blanked() {
            // The shutting-down screen and the trusted prompt (`elevd` asks
            // for a password) are for someone to see.
            if super::powerfeed::active() || self.prompt.is_some() {
                self.wake_screen();
            }
            return;
        }
        if self.idle.due(now) && !super::powerfeed::active() && self.prompt.is_none() {
            BLANKED.store(true, Ordering::Relaxed);
            trace::line!(trace::Channel::Xuid, "IDLE:BLANK");
            self.repaint_full();
            // Keys `inputd` delivers itself stop at the door while it is dark.
            self.push_input();
        }
    }

    /// Note that `event` arrived. `true` when it only woke the screen and is
    /// to be dropped (a press, a key or a wheel notch; moves and releases
    /// carry on, so the cursor follows the hand and nothing stays held).
    pub(super) fn idle_input(&mut self, event: &Event) -> bool {
        let now = sys::clock();
        self.idle.note_input(now);
        let press = matches!(
            event.kind,
            EventKind::PointerDown | EventKind::KeyDown | EventKind::PointerWheel
        );
        if !blanked() {
            return press && self.idle.in_grace(now);
        }
        // `inputd` reports keys itself (`key_activity`): waking on the kernel
        // stream's copy would end the hold before `inputd` dropped the key.
        let key = matches!(event.kind, EventKind::KeyDown | EventKind::KeyUp);
        if key && self.input.link.is_some() {
            return true;
        }
        self.wake_screen();
        press
    }

    /// `inputd` saw a key go down on some keyboard (`ShellEvent::KeyActivity`):
    /// that is activity, and it wakes a blank screen. The key itself was
    /// already dropped by the hold.
    pub(super) fn key_activity(&mut self) {
        self.idle.note_input(sys::clock());
        self.wake_screen();
    }

    /// Light the screen again and give `inputd` the keys back.
    fn wake_screen(&mut self) {
        if !BLANKED.swap(false, Ordering::Relaxed) {
            return;
        }
        let now = sys::clock();
        self.idle.note_input(now);
        self.idle.start_grace(now);
        trace::line!(trace::Channel::Xuid, "IDLE:WAKE");
        self.repaint_full();
        self.push_input();
    }
}

/// Boot check of the timer: never when off, due exactly at the delay, a
/// changed delay restarts the count and input pushes it back.
/// `XUID:IDLE:PASS` or `XUID:IDLE:FAIL`.
pub(super) fn selftest_idle() -> &'static str {
    let minute = TICKS_PER_MINUTE;
    let mut timer = IdleTimer::new(1000);
    // Off by default: no amount of quiet blanks.
    let never = !timer.due(1000 + 1000 * minute);
    // Five minutes: not at 4:59, due at 5:00.
    let changed = timer.set_minutes(5, 1000) && !timer.set_minutes(5, 1000);
    let edge = !timer.due(1000 + 5 * minute - 1) && timer.due(1000 + 5 * minute);
    // Input pushes the deadline out.
    timer.note_input(1000 + 4 * minute);
    let pushed = !timer.due(1000 + 5 * minute) && timer.due(1000 + 9 * minute);
    // Re-setting a different delay starts over from now.
    let restarted =
        timer.set_minutes(1, 5000) && !timer.due(5000 + minute - 1) && timer.due(5000 + minute);
    // A clock that went backwards does not wrap into "long idle".
    let backwards = !timer.due(100);
    // Back to never.
    let off = timer.set_minutes(0, 6000) && !timer.due(u64::MAX);
    // A wake drops presses for the grace period, no longer.
    timer.start_grace(7000);
    let grace = timer.in_grace(7000)
        && timer.in_grace(7000 + GRACE_TICKS - 1)
        && !timer.in_grace(7000 + GRACE_TICKS);
    if never && changed && edge && pushed && restarted && backwards && off && grace {
        "XUID:IDLE:PASS\n"
    } else {
        "XUID:IDLE:FAIL\n"
    }
}
