//! Frame-time markers (docs/lazysky-plan.md §10.3, R7): the painter and the
//! tick drain time themselves; every 2 s while the feed scrolls the numbers
//! go out as one `SKY:PERF` line.

use std::time::{Duration, Instant};

/// How often a line is due while frames are being drawn.
const REPORT_EVERY: Duration = Duration::from_secs(2);
/// The most frame times kept for one report.
const MAX_SAMPLES: usize = 4096;

#[derive(Default)]
pub struct Perf {
    paints_ms: Vec<f32>,
    tick_max_ms: f32,
    last_report: Option<Instant>,
    total_frames: u64,
    total_max_paint_ms: f32,
    total_max_tick_ms: f32,
}

/// One report's numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub p50: f32,
    pub p99: f32,
    pub max: f32,
    pub tick_max: f32,
    pub frames: usize,
}

impl Summary {
    /// `paint_p50=<ms>:paint_p99=<ms>:paint_max=<ms>:tick_max=<ms>:frames=<n>`.
    pub fn fields(&self) -> String {
        format!(
            "paint_p50={:.2}:paint_p99={:.2}:paint_max={:.2}:tick_max={:.2}:frames={}",
            self.p50, self.p99, self.max, self.tick_max, self.frames
        )
    }
}

impl Perf {
    pub fn paint(&mut self, took: Duration) {
        let ms = took.as_secs_f32() * 1000.0;
        if self.paints_ms.len() < MAX_SAMPLES {
            self.paints_ms.push(ms);
        }
        self.total_frames += 1;
        self.total_max_paint_ms = self.total_max_paint_ms.max(ms);
    }

    pub fn tick(&mut self, took: Duration) {
        let ms = took.as_secs_f32() * 1000.0;
        self.tick_max_ms = self.tick_max_ms.max(ms);
        self.total_max_tick_ms = self.total_max_tick_ms.max(ms);
    }

    /// The summary of the frames since the last one, when 2 s have passed
    /// and at least one frame was drawn.
    pub fn due(&mut self, now: Instant) -> Option<Summary> {
        let last = *self.last_report.get_or_insert(now);
        if now.duration_since(last) < REPORT_EVERY || self.paints_ms.is_empty() {
            return None;
        }
        self.last_report = Some(now);
        Some(self.drain())
    }

    /// The summary of everything since the last report, whatever the time.
    pub fn drain(&mut self) -> Summary {
        let mut times = std::mem::take(&mut self.paints_ms);
        times.sort_by(f32::total_cmp);
        let at = |q: f32| {
            let i = ((times.len() as f32 - 1.0) * q).round() as usize;
            times.get(i).copied().unwrap_or(0.0)
        };
        let summary = Summary {
            p50: at(0.5),
            p99: at(0.99),
            max: times.last().copied().unwrap_or(0.0),
            tick_max: self.tick_max_ms,
            frames: times.len(),
        };
        self.tick_max_ms = 0.0;
        summary
    }

    /// The whole session's line, for the close.
    pub fn total_fields(&self) -> String {
        format!(
            "paint_max={:.2}:tick_max={:.2}:frames={}",
            self.total_max_paint_ms, self.total_max_tick_ms, self.total_frames
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_the_reporting_clock() {
        let mut perf = Perf::default();
        let t0 = Instant::now();
        assert!(perf.due(t0).is_none(), "nothing drawn yet");
        for ms in 1..=100 {
            perf.paint(Duration::from_millis(ms));
        }
        perf.tick(Duration::from_millis(3));
        assert!(perf.due(t0 + Duration::from_secs(1)).is_none());
        let s = perf.due(t0 + Duration::from_secs(3)).unwrap();
        assert_eq!(s.frames, 100);
        assert!((s.p50 - 50.0).abs() <= 1.5, "{}", s.p50);
        assert!((s.p99 - 99.0).abs() <= 1.5, "{}", s.p99);
        assert!((s.max - 100.0).abs() < 0.5);
        assert!((s.tick_max - 3.0).abs() < 0.5);
        assert!(s.fields().starts_with("paint_p50="));
        assert!(perf.due(t0 + Duration::from_secs(6)).is_none(), "drained");
        assert!(perf.total_fields().contains("frames=100"));
    }
}
