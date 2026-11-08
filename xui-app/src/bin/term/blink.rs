//! The cursor blink: when it runs, and when it stops. A blinking cursor is a
//! frame twice a second, so it runs only while the window is focused and
//! someone is there: ten seconds without a key or output and the cursor
//! stops steady and on, and the timer is killed, so an idle Terminal costs no
//! frames at all. Pure state, driven by the host's timer and clock.

/// The time between toggles (milliseconds).
pub const PERIOD_MS: u32 = 530;
/// Idle time after which the blink stops.
pub const IDLE_MS: u64 = 10_000;
/// After a key, output or a resize the cursor stays solid this long, so a
/// frame taken just then (a screenshot) always shows it.
pub const SOLID_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug)]
pub struct Blink {
    /// `cursor/blink` and the desktop's motion setting allow blinking.
    allowed: bool,
    focused: bool,
    /// The cursor is drawn this phase.
    on: bool,
    /// The blink stopped itself after the idle time.
    idle: bool,
    last_activity: u64,
}

impl Default for Blink {
    fn default() -> Blink {
        Blink {
            allowed: true,
            focused: true,
            on: true,
            idle: false,
            last_activity: 0,
        }
    }
}

impl Blink {
    /// `cursor/blink`, and whether `sys/ui/motion` is `none` (which forbids
    /// it).
    pub fn configure(&mut self, blink: bool, motion_none: bool, now: u64) {
        self.allowed = blink && !motion_none;
        self.activity(now);
    }

    pub fn set_focused(&mut self, focused: bool, now: u64) {
        self.focused = focused;
        self.activity(now);
    }

    /// A key, a pointer press or output arrived: the cursor shows at once
    /// and the blink starts over.
    pub fn activity(&mut self, now: u64) {
        self.on = true;
        self.idle = false;
        self.last_activity = now;
    }

    /// The timer fired; returns whether the cursor's phase changed (a frame
    /// is due).
    pub fn tick(&mut self, now: u64) -> bool {
        if !self.wants_timer() {
            return false;
        }
        // Solid just after activity: the timer's phase does not count.
        if now.saturating_sub(self.last_activity) < SOLID_MS {
            let changed = !self.on;
            self.on = true;
            return changed;
        }
        if now.saturating_sub(self.last_activity) >= IDLE_MS {
            // Stop steady and visible.
            let changed = !self.on;
            self.on = true;
            self.idle = true;
            return changed;
        }
        self.on = !self.on;
        true
    }

    /// Whether the host should be running the timer.
    pub fn wants_timer(&self) -> bool {
        self.allowed && self.focused && !self.idle
    }

    /// Whether the cursor is drawn this phase.
    pub fn cursor_on(&self) -> bool {
        self.on
    }

    pub fn focused(&self) -> bool {
        self.focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the timer the way the host does until `until` ms; the number of
    /// frames it caused.
    fn frames(blink: &mut Blink, from: u64, until: u64) -> u32 {
        let mut frames = 0;
        let mut now = from;
        while now < until && blink.wants_timer() {
            now += u64::from(PERIOD_MS);
            if blink.tick(now) {
                frames += 1;
            }
        }
        frames
    }

    #[test]
    fn it_blinks_while_someone_is_there() {
        let mut blink = Blink::default();
        blink.activity(0);
        assert!(blink.cursor_on());
        // Solid for the first second, then it blinks.
        assert!(!blink.tick(530) && blink.cursor_on());
        assert!(blink.tick(1060) && !blink.cursor_on());
        assert!(blink.tick(1590) && blink.cursor_on());
    }

    #[test]
    fn activity_keeps_it_solid_for_a_second_whatever_the_timer_phase() {
        let mut blink = Blink::default();
        blink.activity(0);
        // The timer was mid-phase when a key arrived at 1200 ms: its next
        // tick, 10 ms later, must not blank the cursor.
        assert!(blink.tick(1060) && !blink.cursor_on());
        blink.activity(1200);
        assert!(blink.cursor_on());
        assert!(!blink.tick(1210), "still solid");
        assert!(blink.cursor_on());
        assert!(blink.tick(2300) && !blink.cursor_on(), "blinking again after the solid second");
    }

    #[test]
    fn an_idle_terminal_stops_blinking_and_costs_no_frames() {
        let mut blink = Blink::default();
        blink.activity(0);
        let busy = frames(&mut blink, 0, 60_000);
        assert!(busy <= 21, "about ten seconds of blinks, not a minute: {busy}");
        assert!(!blink.wants_timer(), "the timer is killed");
        assert!(blink.cursor_on(), "it stops steady and visible");
        // A minute more of host timer ticks that never come: nothing to draw.
        assert!(!blink.tick(120_000));
        assert_eq!(frames(&mut blink, 60_000, 120_000), 0);
    }

    #[test]
    fn activity_restarts_the_blink() {
        let mut blink = Blink::default();
        blink.activity(0);
        frames(&mut blink, 0, 30_000);
        assert!(!blink.wants_timer());
        blink.activity(31_000);
        assert!(blink.wants_timer() && blink.cursor_on());
        assert!(frames(&mut blink, 31_000, 36_000) > 5);
    }

    #[test]
    fn it_runs_only_while_focused() {
        let mut blink = Blink::default();
        blink.set_focused(false, 0);
        assert!(!blink.wants_timer());
        assert!(!blink.tick(530));
        blink.set_focused(true, 1000);
        assert!(blink.wants_timer());
    }

    #[test]
    fn it_does_not_blink_when_configured_off_or_motion_is_none() {
        let mut blink = Blink::default();
        blink.configure(false, false, 0);
        assert!(!blink.wants_timer() && blink.cursor_on());
        blink.configure(true, true, 0);
        assert!(!blink.wants_timer(), "sys/ui/motion = none");
        blink.configure(true, false, 0);
        assert!(blink.wants_timer());
    }

    #[test]
    fn going_idle_while_off_turns_the_cursor_back_on() {
        let mut blink = Blink::default();
        blink.activity(0);
        assert!(blink.tick(1060) && !blink.cursor_on());
        // The next look is past the idle time with the cursor off: one last
        // frame turns it on.
        assert!(blink.tick(10_000));
        assert!(blink.cursor_on() && !blink.wants_timer());
    }
}
