//! Frame pacing for the flying game: the frame rate the HUD shows, the
//! resolution adaptation it feeds, and the benchmark flyover that measures
//! it.

use std::time::{Duration, Instant};

use crate::fly::{yaw_of, EYE};
use crate::math::{polyline_at, Vec3};
use crate::scale::adapt;

use super::{Report, Running};

const BENCH_SECONDS: f32 = 24.0;
/// A gap between drawn frames longer than this is a pause, not a slow frame.
const IDLE_GAP: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct Stats {
    pub(super) window_start: Option<Instant>,
    /// When the last frame was drawn: a pause longer than [`IDLE_GAP`]
    /// starts a new window, so a still camera never counts as slow frames.
    pub(super) last_frame: Option<Instant>,
    pub(super) frames: u32,
    pub(super) work: f32,
    /// The last second's numbers, for the HUD.
    pub(super) fps: u32,
    pub(super) work_ms: f32,
}

pub(super) struct Bench {
    pub(super) started: Instant,
    pub(super) seconds: Vec<u32>,
    pub(super) frames: u32,
}

impl Running {
    /// Counts a drawn frame; once a second reports and adapts the scale.
    pub(super) fn count(&mut self, work_ms: f32, now: Instant, reports: &mut Vec<Report>) {
        let stats = &mut self.stats;
        if stats
            .last_frame
            .is_some_and(|last| now.duration_since(last) > IDLE_GAP)
        {
            // The camera rested: drop the partial window instead of reading
            // the pause as a frame rate.
            stats.window_start = None;
            stats.frames = 0;
            stats.work = 0.0;
        }
        stats.last_frame = Some(now);
        let start = *stats.window_start.get_or_insert(now);
        stats.frames += 1;
        stats.work += work_ms;
        if let Some(bench) = self.bench.as_mut() {
            bench.frames += 1;
        }
        let elapsed = now.duration_since(start).as_secs_f32();
        if elapsed < 1.0 {
            return;
        }
        stats.fps = (stats.frames as f32 / elapsed).round() as u32;
        stats.work_ms = stats.work / stats.frames as f32;
        stats.window_start = Some(now);
        stats.frames = 0;
        stats.work = 0.0;
        let (fps, work) = (stats.fps, stats.work_ms);
        if let Some(bench) = self.bench.as_mut() {
            bench.seconds.push(fps);
        }
        reports.push(Report::Fps {
            fps,
            scale: self.scale,
            width: self.frame.width,
            height: self.frame.height,
            work_ms: work,
        });
        if self.auto_scale {
            let scale = adapt(self.scale, work, fps);
            if scale != self.scale {
                // Draw the next frame at the new scale; this one keeps its own.
                self.scale = scale;
                self.dirty = true;
            }
        }
    }

    /// The benchmark: fly every hole tee to green at 35 m, timed.
    pub(super) fn fly_bench(&mut self, now: Instant, reports: &mut Vec<Report>) {
        let bench = self.bench.as_ref().expect("benching");
        let t = now.duration_since(bench.started).as_secs_f32() / BENCH_SECONDS;
        if t >= 1.0 {
            let bench = self.bench.take().expect("benching");
            // The first second warms up (the scale settles).
            let counted = &bench.seconds[bench.seconds.len().min(1)..];
            let min = counted.iter().copied().min().unwrap_or(0);
            let avg = counted.iter().sum::<u32>() as f32 / counted.len().max(1) as f32;
            reports.push(Report::Bench {
                frames: bench.frames,
                min_fps: min,
                avg_fps: avg,
            });
            return;
        }
        let holes = &self.scene.course.holes;
        let along = t * holes.len() as f32;
        let hole = &holes[(along as usize).min(holes.len() - 1)];
        let (p, dir) = polyline_at(&hole.centerline, along.fract() * hole.length_m);
        let ground = self.scene.bake.height_at(p.x, p.y);
        let cam = &mut self.flyer.camera;
        cam.eye = Vec3::new(p.x, ground + 35.0 + EYE, p.y);
        cam.yaw = yaw_of(dir);
        cam.pitch = -0.3;
        self.moved();
    }
}
