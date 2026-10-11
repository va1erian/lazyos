//! The game's state from one timer tick to the next: waiting for the course
//! (generated on a worker thread, `loading.rs`), then flying over it. Each tick moves the camera,
//! renders when anything changed and turns the frame into the image the
//! window shows. The internal resolution adapts so frames stay above 20 per
//! second; *authentic* mode repaints the view progressively once the camera
//! stops, the way a 486 did.

use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

mod pacing;
use pacing::{Bench, Stats};

use xui_core::Image;

use crate::fly::{Flyer, Held};
use crate::gen::Params;
use crate::loading::Loading;
use crate::math::Vec3;
use crate::minimap::Minimap;
use crate::play::Play;
use crate::render::{resolve_into, BallMark, Camera, Frame, Mode, Overlay, RenderJob, Scene};

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
/// How fast a held key turns the aim, radians per second.
const AIM_RATE: f32 = 0.3;
/// The flying part, once a course exists.
pub struct Running {
    pub scene: Scene,
    /// The free camera: the practice and spectator view (`F`), and the
    /// benchmark's.
    pub flyer: Flyer,
    /// The hole being played: the camera follows its ball while `free` is
    /// off.
    pub play: Play,
    /// Whether the free camera is in use instead of the shot camera.
    pub free: bool,
    /// The camera the last frame was drawn from.
    camera: Camera,
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
    overlay: Overlay,
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

pub struct Game {
    params: Params,
    loading: Option<Loading>,
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
            loading: Some(Loading::start(params)),
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
        self.loading.as_ref().map(|l| {
            let (label, done) = l.snapshot();
            (
                format!("{label} - {} s", l.started.elapsed().as_secs()),
                done,
            )
        })
    }

    pub fn seed(&self) -> u64 {
        self.params.seed
    }

    /// Starts generating a new course from `seed`.
    pub fn regenerate(&mut self, seed: u64) {
        self.params = Params::new(seed);
        self.loading = Some(Loading::start(self.params));
        self.run = None;
    }

    pub fn take_report(&mut self) -> Option<Report> {
        (!self.reports.is_empty()).then(|| self.reports.remove(0))
    }

    /// One timer tick for a `width` x `height` (device pixels) view.
    /// Returns whether the picture changed.
    pub fn tick(&mut self, width: usize, height: usize, now: Instant) -> bool {
        if let Some(loading) = self.loading.as_mut() {
            match loading.rx.try_recv() {
                Ok(course) => {
                    let millis = loading.started.elapsed().as_millis();
                    let scene = Scene::new(course);
                    self.reports.push(Report::Ready {
                        name: scene.course.name.clone(),
                        seed: scene.course.seed,
                        par: scene.course.par,
                        millis,
                    });
                    self.run = Some(Running::new(scene, self.map_size, width, now));
                    self.loading = None;
                    return true;
                }
                Err(TryRecvError::Disconnected) => loading.failed = true,
                Err(TryRecvError::Empty) => {}
            }
            // The elapsed seconds in the label change once a second.
            let now_shown = self.loading().unwrap_or_default();
            let loading = self.loading.as_mut().expect("still loading");
            let changed = loading.shown != now_shown;
            loading.shown = now_shown;
            return changed;
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
        let play = Play::tee_off(&scene.course.holes, &scene.bake, 0);
        let camera = flyer.camera;
        let mut run = Running {
            scene,
            flyer,
            play,
            free: false,
            camera,
            frame: Frame::new(1, 1),
            frame_scale: 1,
            image: None,
            minimap,
            // Native resolution up to about 1280 wide; the adaptation
            // coarsens it if frames fall too slow (`scale.rs`).
            scale: (width as f32 / 1280.0).round().max(1.0) as usize,
            auto_scale: true,
            authentic: false,
            job: None,
            overlay: Overlay::default(),
            dirty: true,
            settled: true,
            last_input: now,
            last_tick: now,
            last_water: now,
            started: now,
            size: (0, 0),
            stats: Stats::default(),
            bench: None,
        };
        run.sync_play();
        run
    }

    /// The camera the view is drawn from: the free camera, or the shot
    /// camera for the ball in play.
    pub fn camera(&self) -> Camera {
        self.camera
    }

    /// Shows the ball and the aim for the current play state and redraws.
    pub fn sync_play(&mut self) {
        let bake = &self.scene.bake;
        let ball = self.play.ball;
        self.overlay.ball = Some(BallMark {
            position: ball,
            ground: bake.height_at(ball.x, ball.z),
        });
        self.overlay.aim = Some(self.play.aim_mark(bake));
        self.moved();
    }

    /// Plays hole `n` from its tee.
    pub fn start_hole(&mut self, n: usize) {
        self.play = Play::tee_off(&self.scene.course.holes, &self.scene.bake, n);
        self.free = false;
        self.sync_play();
    }

    /// Switches between the shot camera and the free one.
    pub fn toggle_free(&mut self) {
        self.free = !self.free;
        if self.free {
            // Fly on from where the shot camera was.
            self.flyer.camera = self.camera;
            self.overlay.aim = None;
            self.moved();
        } else {
            self.sync_play();
        }
    }

    /// Something moved the camera (a key, the mouse, a teleport).
    pub fn moved(&mut self) {
        self.dirty = true;
        self.last_input = Instant::now();
        self.job = None;
    }

    /// Shows the ball at `position` (`None`: no ball), drawn over every view.
    pub fn set_ball(&mut self, position: Option<Vec3>) {
        let ball = position.map(|position| BallMark {
            position,
            ground: self.scene.bake.height_at(position.x, position.z),
        });
        if ball != self.overlay.ball {
            self.overlay.ball = ball;
            self.moved();
        }
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
        if !self.free {
            self.toggle_free();
        }
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
        } else if !self.free {
            self.aim_tick(held, dt);
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

    /// Held turn keys rotate the aim (Shift for a fast turn).
    fn aim_tick(&mut self, held: Held, dt: f32) {
        use crate::fly::Action;
        let turn = |plus: Action, minus: Action| {
            f32::from(u8::from(held.has(plus))) - f32::from(u8::from(held.has(minus)))
        };
        let axis = turn(Action::TurnRight, Action::TurnLeft) + turn(Action::Right, Action::Left);
        if axis != 0.0 {
            let fast = if held.has(Action::Fast) { 4.0 } else { 1.0 };
            self.play
                .rotate(axis.clamp(-1.0, 1.0) * AIM_RATE * fast * dt);
            self.sync_play();
        }
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
        let camera = self.shot_camera();
        self.camera = camera;
        let mut job = RenderJob::new(&self.scene, &camera, w, h, mode, time)
            .with_overlay(self.overlay.clone());
        job.step(&mut self.frame, &self.scene, None);
        self.count(start.elapsed().as_secs_f32() * 1000.0, now, reports);
    }

    /// The camera for the next frame.
    fn shot_camera(&self) -> Camera {
        if self.free || self.bench.is_some() {
            return self.flyer.camera;
        }
        let aspect = self.size.1 as f32 / self.size.0.max(1) as f32;
        self.play.camera(&self.scene.bake, aspect)
    }

    /// Authentic mode: a few more chunks of the progressive repaint.
    fn paint_in(&mut self, time: f32) -> bool {
        if self.job.is_none() {
            let camera = self.camera;
            let (w, h) = (self.frame.width, self.frame.height);
            self.job = Some(
                RenderJob::new(
                    &self.scene,
                    &camera,
                    w,
                    h,
                    Mode::Authentic { chunks: 3 },
                    time,
                )
                .with_overlay(self.overlay.clone()),
            );
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

    /// Turns the framebuffer into the window-sized image. The last image's
    /// pixels are filled again in place: a window-sized buffer is big enough
    /// to be an `mmap` and a page fault per page each frame otherwise.
    fn present(&mut self) {
        let (w, h) = self.size;
        let mut rgba = self
            .image
            .take()
            .map(Image::into_pixels)
            .unwrap_or_default();
        resolve_into(
            &self.frame,
            &self.scene.palette.words(),
            self.frame_scale,
            w,
            h,
            &mut rgba,
        );
        self.image = Image::from_rgba(w as u32, h as u32, rgba).ok();
    }
}
