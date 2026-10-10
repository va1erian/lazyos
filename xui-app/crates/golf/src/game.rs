//! The game's state from one timer tick to the next: generating the course
//! a stage per tick, then flying over it. Each tick moves the camera,
//! renders when anything changed and turns the frame into the image the
//! window shows. The internal resolution adapts so frames stay above 20 per
//! second; *authentic* mode repaints the view progressively once the camera
//! stops, the way a 486 did.

use std::time::{Duration, Instant};

use xui_core::Image;

use crate::fly::{yaw_of, Flyer, Held, EYE};
use crate::gen::{Generator, Params};
use crate::math::{polyline_at, Vec3};
use crate::minimap::Minimap;
use crate::render::{resolve, Camera, Frame, Mode, RenderJob, Scene};

/// What the game tells its host (the LazyOS binary prints it as serial
/// evidence).
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// The course is generated and the first view drawn.
    Ready {
        name: String,
        seed: u64,
        par: u8,
        millis: u128,
    },
    /// Once a second while frames are drawn.
    Fps {
        fps: u32,
        scale: usize,
        width: usize,
        height: usize,
        work_ms: f32,
    },
    /// The benchmark flyover finished.
    Bench {
        frames: u32,
        min_fps: u32,
        avg_fps: f32,
    },
}

/// The overhead map's size, pixels at 96 DPI.
pub const MAP_SIZE: usize = 200;
/// How often the water's palette animation advances: often enough to look
/// continuous (each step only re-resolves the frame).
const WATER_PERIOD: Duration = Duration::from_millis(60);
const BENCH_SECONDS: f32 = 24.0;
/// Frame work above this (ms) coarsens the internal resolution.
const SLOW_MS: f32 = 40.0;
/// Fewer frames a second than this coarsens it too (the floor is 20).
const MIN_FPS: u32 = 22;

/// The flying part, once a course exists.
pub struct Running {
    pub scene: Scene,
    pub flyer: Flyer,
    frame: Frame,
    /// The scale `frame` was rendered at: it is always presented at that
    /// scale, even after `scale` has moved on, until a new frame replaces it.
    frame_scale: usize,
    pub image: Option<Image>,
    pub minimap: Image,
    /// Window pixels per framebuffer pixel.
    pub scale: usize,
    /// Whether the scale follows the frame cost.
    pub auto_scale: bool,
    pub authentic: bool,
    job: Option<RenderJob>,
    dirty: bool,
    settled: bool,
    last_input: Instant,
    last_tick: Instant,
    last_water: Instant,
    started: Instant,
    size: (usize, usize),
    stats: Stats,
    bench: Option<Bench>,
}

#[derive(Default)]
struct Stats {
    window_start: Option<Instant>,
    frames: u32,
    work: f32,
    /// The last second's numbers, for the HUD.
    fps: u32,
    work_ms: f32,
}

struct Bench {
    started: Instant,
    seconds: Vec<u32>,
    frames: u32,
}

pub struct Game {
    params: Params,
    loading: Option<(Generator, Instant)>,
    pub run: Option<Running>,
    pub held: Held,
    pub show_help: bool,
    pub show_map: bool,
    reports: Vec<Report>,
    map_size: usize,
}

impl Game {
    /// A game that generates the course for `seed`; `dpi` sizes the map.
    pub fn new(seed: u64, dpi: u32) -> Game {
        Game::with_params(Params::new(seed), dpi)
    }

    /// A game for `params` (tests use a quick, unsearched course).
    pub fn with_params(params: Params, dpi: u32) -> Game {
        Game {
            params,
            loading: Some((Generator::new(params), Instant::now())),
            run: None,
            held: Held::default(),
            show_help: true,
            show_map: true,
            reports: Vec::new(),
            map_size: MAP_SIZE * dpi.max(96) as usize / 96,
        }
    }

    /// What the generator is doing and how far along it is, while it runs.
    pub fn loading(&self) -> Option<(String, f32)> {
        self.loading.as_ref().map(|(g, _)| g.progress())
    }

    pub fn seed(&self) -> u64 {
        self.params.seed
    }

    /// Starts generating a new course from `seed`.
    pub fn regenerate(&mut self, seed: u64) {
        self.params = Params::new(seed);
        self.loading = Some((Generator::new(self.params), Instant::now()));
        self.run = None;
    }

    pub fn take_report(&mut self) -> Option<Report> {
        (!self.reports.is_empty()).then(|| self.reports.remove(0))
    }

    /// One timer tick for a `width` x `height` (device pixels) view.
    /// Returns whether the picture changed.
    pub fn tick(&mut self, width: usize, height: usize, now: Instant) -> bool {
        if let Some((generator, started)) = self.loading.as_mut() {
            if let Some(course) = generator.step() {
                let millis = started.elapsed().as_millis();
                let scene = Scene::new(course);
                self.reports.push(Report::Ready {
                    name: scene.course.name.clone(),
                    seed: scene.course.seed,
                    par: scene.course.par,
                    millis,
                });
                self.run = Some(Running::new(scene, self.map_size, width, now));
                self.loading = None;
            }
            return true;
        }
        let held = self.held;
        match self.run.as_mut() {
            Some(run) => run.tick(held, width, height, now, &mut self.reports),
            None => false,
        }
    }
}

impl Running {
    fn new(scene: Scene, map_size: usize, width: usize, now: Instant) -> Running {
        let map = Minimap::new(&scene.course, map_size);
        let minimap =
            Image::from_rgba(map.width as u32, map.height as u32, map.rgba).expect("map size");
        let mut flyer = Flyer::new(Camera::new(Vec3::default(), 0.0, 0.0));
        if let Some(first) = scene.course.holes.first() {
            flyer.to_tee(first, |x, z| scene.bake.height_at(x, z));
        }
        Running {
            scene,
            flyer,
            frame: Frame::new(1, 1),
            frame_scale: 1,
            image: None,
            minimap,
            // Native resolution up to about 1280 wide; the adaptation
            // coarsens it if frames fall under MIN_FPS.
            scale: (width as f32 / 1280.0).round().max(1.0) as usize,
            auto_scale: true,
            authentic: false,
            job: None,
            dirty: true,
            settled: true,
            last_input: now,
            last_tick: now,
            last_water: now,
            started: now,
            size: (0, 0),
            stats: Stats::default(),
            bench: None,
        }
    }

    /// Something moved the camera (a key, the mouse, a teleport).
    pub fn moved(&mut self) {
        self.dirty = true;
        self.last_input = Instant::now();
        self.job = None;
    }

    /// Cycles the scale: automatic, then 1 to 4 window pixels per pixel.
    pub fn cycle_scale(&mut self) {
        (self.auto_scale, self.scale) = match (self.auto_scale, self.scale) {
            (true, _) => (false, 1),
            (false, s) if s >= 4 => (true, self.scale),
            (false, s) => (false, s + 1),
        };
        self.moved();
    }

    pub fn start_bench(&mut self) {
        self.bench = Some(Bench {
            started: Instant::now(),
            seconds: Vec::new(),
            frames: 0,
        });
        self.auto_scale = true;
    }

    pub fn benching(&self) -> bool {
        self.bench.is_some()
    }

    /// The framebuffer's size.
    pub fn resolution(&self) -> (usize, usize) {
        (self.frame.width, self.frame.height)
    }

    pub fn fps(&self) -> (u32, f32) {
        (self.stats.fps, self.stats.work_ms)
    }

    fn tick(
        &mut self,
        held: Held,
        width: usize,
        height: usize,
        now: Instant,
        reports: &mut Vec<Report>,
    ) -> bool {
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        if width == 0 || height == 0 {
            return false;
        }
        if self.size != (width, height) {
            self.size = (width, height);
            self.dirty = true;
        }
        let scene = &self.scene;
        let size = scene.bake.size as f32;
        if self.bench.is_some() {
            self.fly_bench(now, reports);
        } else if self
            .flyer
            .update(held, dt, size, |x, z| scene.bake.height_at(x, z))
        {
            self.moved();
        }
        let time = now.duration_since(self.started).as_secs_f32();
        let quiet = now.duration_since(self.last_input) > Duration::from_millis(300);
        let drew = if self.dirty {
            self.dirty = false;
            self.settled = !self.authentic;
            self.draw(Mode::Fly, time, now, reports);
            true
        } else if self.authentic && !self.settled && quiet {
            self.paint_in(time)
        } else {
            false
        };
        // The water cycles even when nothing else moves: only the palette
        // changes, so the frame is resolved again, not re-rendered.
        let water = now.duration_since(self.last_water) >= WATER_PERIOD;
        if water {
            self.last_water = now;
            let seconds = now.duration_since(self.started).as_secs_f32();
            self.scene.palette.animate_water(seconds);
        }
        if drew || water {
            self.present();
        }
        drew || water
    }

    /// Renders a whole frame and keeps the books on how long it took.
    fn draw(&mut self, mode: Mode, time: f32, now: Instant, reports: &mut Vec<Report>) {
        let start = Instant::now();
        let (w, h) = (
            self.size.0.div_ceil(self.scale),
            self.size.1.div_ceil(self.scale),
        );
        self.frame_scale = self.scale;
        if (self.frame.width, self.frame.height) != (w, h) {
            self.frame = Frame::new(w, h);
        }
        let camera = self.flyer.camera;
        let mut job = RenderJob::new(&self.scene, &camera, w, h, mode, time);
        job.step(&mut self.frame, &self.scene, None);
        self.count(start.elapsed().as_secs_f32() * 1000.0, now, reports);
    }

    /// Authentic mode: a few more chunks of the progressive repaint.
    fn paint_in(&mut self, time: f32) -> bool {
        if self.job.is_none() {
            let camera = self.flyer.camera;
            let (w, h) = (self.frame.width, self.frame.height);
            self.job = Some(RenderJob::new(
                &self.scene,
                &camera,
                w,
                h,
                Mode::Authentic { chunks: 3 },
                time,
            ));
        }
        let job = self.job.as_mut().expect("just set");
        if job.step(
            &mut self.frame,
            &self.scene,
            Some(Duration::from_millis(12)),
        ) {
            self.job = None;
            self.settled = true;
        }
        true
    }

    /// Turns the framebuffer into the window-sized image.
    fn present(&mut self) {
        let (w, h) = self.size;
        let rgba = resolve(
            &self.frame,
            &self.scene.palette.words(),
            self.frame_scale,
            w,
            h,
        );
        self.image = Image::from_rgba(w as u32, h as u32, rgba).ok();
    }

    /// Counts a drawn frame; once a second reports and adapts the scale.
    fn count(&mut self, work_ms: f32, now: Instant, reports: &mut Vec<Report>) {
        let stats = &mut self.stats;
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
    fn fly_bench(&mut self, now: Instant, reports: &mut Vec<Report>) {
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

/// The next scale: coarser when a frame costs too much or too few frames
/// are drawn, finer when there is room. The pixel count grows with
/// (s / (s-1))^2 but the cost grows less (sky, sprites and haze scale
/// sublinearly), so the estimate errs on the safe side.
pub fn adapt(scale: usize, work_ms: f32, fps: u32) -> usize {
    if (work_ms > SLOW_MS || (fps > 0 && fps < MIN_FPS)) && scale < 6 {
        scale + 1
    } else if scale > 1 && fps >= 30 {
        let ratio = scale as f32 / (scale - 1) as f32;
        if work_ms * ratio * ratio < SLOW_MS * 0.8 {
            scale - 1
        } else {
            scale
        }
    } else {
        scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_adapts_both_ways() {
        assert_eq!(adapt(2, 45.0, 18), 3);
        assert_eq!(adapt(2, 10.0, 15), 3, "too few frames is slow too");
        assert_eq!(adapt(3, 5.0, 40), 2);
        assert_eq!(
            adapt(2, 7.0, 60),
            1,
            "QEMU measured 46+ fps at 1x after 7 ms at 2x"
        );
        assert_eq!(adapt(2, 12.0, 40), 2, "finer would cost about 48 ms");
        assert_eq!(adapt(1, 2.0, 60), 1);
        assert_eq!(adapt(6, 90.0, 5), 6);
    }
}
