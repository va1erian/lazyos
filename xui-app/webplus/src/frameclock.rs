//! What the window thread spends on the page, once a second: `INFO:WINDOW:`.
//!
//! The UI process counts its own paint (`INFO:PAINT`); this is the other half, on the window's
//! thread: applying what the engine posted to the picture (`apply`), and drawing it (`draw`, where
//! xui prepares an image it has not seen before: the cost of a whole new frame, which a scroll's
//! delta avoids). Printed only while something happened.
//!
//!   `INFO:WINDOW:updates=<n>:full=<whole frames>:apply=<us, summed>:draws=<n>:draw=<us, summed>:`
//!   `max=<longest apply+draw us>:parts=<most images in the picture>`

use std::time::{Duration, Instant};

const PERIOD: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct FrameClock {
    since: Option<Instant>,
    updates: u32,
    fulls: u32,
    apply_us: u64,
    draws: u32,
    draw_us: u64,
    max_us: u64,
    parts: usize,
}

impl FrameClock {
    /// One update applied at `now`: it took `apply`, whole or not, and the picture is `parts`
    /// images now; `drawn` is the time and count of draws since the last update. Returns the line
    /// to print when a second has passed.
    pub fn record(
        &mut self,
        now: Instant,
        apply: Duration,
        full: bool,
        parts: usize,
        drawn: (u64, u32),
    ) -> Option<String> {
        let since = *self.since.get_or_insert(now);
        self.updates += 1;
        self.fulls += u32::from(full);
        let apply_us = apply.as_micros() as u64;
        self.apply_us += apply_us;
        self.draws += drawn.1;
        self.draw_us += drawn.0;
        self.max_us = self.max_us.max(apply_us + drawn.0);
        self.parts = self.parts.max(parts);
        if now.duration_since(since) < PERIOD {
            return None;
        }
        let line = format!(
            "INFO:WINDOW:updates={}:full={}:apply={}:draws={}:draw={}:max={}:parts={}",
            self.updates,
            self.fulls,
            self.apply_us,
            self.draws,
            self.draw_us,
            self.max_us,
            self.parts
        );
        *self = FrameClock {
            since: Some(now),
            ..FrameClock::default()
        };
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_of_updates_is_one_line_and_then_the_counts_start_again() {
        let start = Instant::now();
        let mut clock = FrameClock::default();
        let ms = Duration::from_millis;
        assert_eq!(clock.record(start, ms(2), true, 1, (900, 1)), None);
        assert_eq!(
            clock.record(start + ms(400), ms(1), false, 4, (300, 1)),
            None
        );
        let line = clock
            .record(start + ms(1000), ms(1), false, 3, (100, 1))
            .unwrap();
        assert_eq!(
            line,
            "INFO:WINDOW:updates=3:full=1:apply=4000:draws=3:draw=1300:max=2900:parts=4"
        );
        assert_eq!(
            clock.record(start + ms(1200), ms(1), false, 2, (50, 1)),
            None
        );
        let next = clock
            .record(start + ms(2100), ms(1), false, 2, (50, 1))
            .unwrap();
        assert!(next.starts_with("INFO:WINDOW:updates=2:full=0:"), "{next}");
    }
}
