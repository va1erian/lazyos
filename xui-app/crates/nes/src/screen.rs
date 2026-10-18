//! The picture: a custom control that paints the console's frame, clocks the
//! console, and takes the keyboard.
//!
//! The frame clock runs on the control's own timer (like the About window's
//! scene): each tick the [`Pacer`](crate::pacer) says how many display frames
//! are due — bounded by the wall clock and by the mixer's room — the session
//! clocks them, the samples go to the [`Host`], and the picture is scaled
//! into an [`Image`] the canvas blits 1:1 (its fast path). The control owns
//! the [`Session`], so nothing else can clock a frame while a paint is in
//! flight.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, WidgetId};
use xui_core::geometry::Size;
use xui_core::image::Image;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Canvas, Color, Dip, Rect, TextStyle};

use crate::app::Msg;
use crate::audio;
use crate::host::{Host, Report};
use crate::input::{self, Pad};
use crate::pacer::Pacer;
use crate::session::Session;
use crate::video::{Aspect, Scale, Scaler};

/// How often the console is offered a frame; the pacer decides whether it
/// runs, so this only sets how smoothly a frame is picked up.
const TICK_MS: u32 = 8;
/// Frames after which the app reports that the console is running.
pub const RUN_EVIDENCE_FRAMES: u64 = 120;
/// How often (in frames, about 30 s) a battery-backed cartridge is written
/// while playing.
const BATTERY_SAVE_FRAMES: u64 = 1800;

/// The NES picture and its clock.
pub struct Screen {
    control: Control<Msg>,
    state: Rc<RefCell<State>>,
}

impl Placeable<Msg> for Screen {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        // The picture takes whatever the layout gives it.
        Size::new(0, 0)
    }
}

impl Screen {
    /// Builds the control and starts its clock.
    pub fn new(ui: &Ui<Msg>, host: Rc<RefCell<dyn Host>>, session: Session) -> Screen {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )
        .expect("the NES screen control");
        let state = Rc::new(RefCell::new(State::new(host, session)));
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.paint(canvas);
                }
            }));
        }
        {
            let state = Rc::clone(&state);
            control.on_events(move |event| {
                let Ok(mut state) = state.try_borrow_mut() else {
                    return None;
                };
                state.event(event)
            });
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            control.set_timer(TICK_MS, move || {
                let mut state = state.try_borrow_mut().ok()?;
                let changed = state.tick();
                let reports = state.take_reports();
                drop(state);
                for report in reports {
                    ui.emit(Msg::Report(report));
                }
                if changed {
                    ui.invalidate(id);
                }
                None
            });
        }
        control.focus();
        Screen { control, state }
    }

    /// The widget, for the layout.
    pub fn control(&self) -> &Control<Msg> {
        &self.control
    }

    /// The session and its clock, for the menu commands.
    pub fn state(&self) -> Rc<RefCell<State>> {
        Rc::clone(&self.state)
    }
}

/// Everything the screen owns.
pub struct State {
    host: Rc<RefCell<dyn Host>>,
    session: Session,
    pad: Pad,
    pacer: Pacer,
    scaler: Scaler,
    /// The scaled picture, ready for the canvas.
    image: Option<Image>,
    /// Whether the console's frame has changed since the last scale.
    picture_dirty: bool,
    /// Whether the fast-forward menu item is on (the key is the pad's).
    fast_forward: bool,
    scale: Scale,
    aspect: Aspect,
    show_fps: bool,
    /// When the last tick ran, for the pacer.
    last: Option<Instant>,
    /// Frames since the last [`State::fps`] reading.
    frames_since_reading: u64,
    /// Frames clocked since the last battery save.
    frames_since_save: u64,
    reported_running: bool,
    reports: Vec<Report>,
    /// The frame's samples as `S16Le`, reused every frame.
    samples: Vec<i16>,
    /// The error the console last reported, drawn over the picture.
    failure: Option<String>,
}

impl State {
    fn new(host: Rc<RefCell<dyn Host>>, session: Session) -> State {
        State {
            host,
            session,
            pad: Pad::new(),
            pacer: Pacer::new(),
            scaler: Scaler::new(),
            image: None,
            picture_dirty: true,
            fast_forward: false,
            scale: Scale::Auto,
            aspect: Aspect::Pixel8x7,
            show_fps: false,
            last: None,
            frames_since_reading: 0,
            frames_since_save: 0,
            reported_running: false,
            reports: Vec::new(),
            samples: Vec::new(),
            failure: None,
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    pub fn set_scale(&mut self, scale: Scale) {
        self.scale = scale;
        self.picture_dirty = true;
    }

    pub fn aspect(&self) -> Aspect {
        self.aspect
    }

    pub fn set_aspect(&mut self, aspect: Aspect) {
        self.aspect = aspect;
        self.picture_dirty = true;
    }

    pub fn show_fps(&self) -> bool {
        self.show_fps
    }

    pub fn set_show_fps(&mut self, show: bool) {
        self.show_fps = show;
    }

    pub fn fast_forward(&self) -> bool {
        self.fast_forward
    }

    pub fn set_fast_forward(&mut self, on: bool) {
        self.fast_forward = on;
    }

    /// The frames clocked in the last second, and starts a new count.
    pub fn fps(&mut self) -> u64 {
        std::mem::take(&mut self.frames_since_reading)
    }

    /// Loads a cartridge: pauses the pacer, reports what the header says.
    pub fn load_rom(&mut self, path: &std::path::Path) -> bool {
        match self.session.load_rom(path) {
            Ok(info) => {
                self.pacer.reset();
                self.picture_dirty = true;
                self.failure = None;
                self.reported_running = false;
                let _ = crate::paths::push_recent(&self.host.borrow().app_dir(), path);
                if let Some(warning) = self.session.take_warning() {
                    self.reports.push(Report::Failed(warning));
                }
                self.reports.push(Report::Loaded {
                    path: info.path.clone(),
                    mapper: info.mapper,
                    prg_kb: info.prg_kb,
                    chr_kb: info.chr_kb,
                    battery: info.battery,
                    region: crate::rom::region_name(info.region),
                });
                true
            }
            Err(error) => {
                self.reports.push(Report::Failed(error.to_string()));
                false
            }
        }
    }

    /// Closes the cartridge, writing its battery first.
    pub fn unload_rom(&mut self) {
        self.session.unload();
        self.image = None;
        self.picture_dirty = true;
    }

    /// Saves the battery file now, reporting where it went.
    pub fn save_battery(&mut self) {
        match self.session.save_battery() {
            Ok(Some(path)) => self.reports.push(Report::Battery { path }),
            Ok(None) => {}
            Err(error) => self.reports.push(Report::Failed(error.to_string())),
        }
    }

    /// Saves or restores a save-state slot.
    pub fn save_state(&mut self, slot: usize) {
        match self.session.save_state(slot) {
            Ok(path) => self.reports.push(Report::StateSaved { slot, path }),
            Err(error) => self.reports.push(Report::Failed(error.to_string())),
        }
    }

    pub fn load_state(&mut self, slot: usize) {
        match self.session.load_state(slot) {
            Ok(path) => {
                self.picture_dirty = true;
                self.reports.push(Report::StateLoaded { slot, path });
            }
            Err(error) => self.reports.push(Report::Failed(error.to_string())),
        }
    }

    /// The reports raised since the last call.
    pub fn take_reports(&mut self) -> Vec<Report> {
        std::mem::take(&mut self.reports)
    }

    /// One timer tick: clock what is due and scale the picture. Public so a
    /// test can drive the console without waiting on the control's timer.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last.unwrap_or(now));
        self.last = Some(now);
        if !self.session.is_loaded() || self.session.paused() {
            self.pacer.reset();
            return false;
        }
        self.pad.tick();
        self.session
            .set_fast_forward(self.fast_forward || self.pad.fast_forward());
        let free = self.host.borrow_mut().audio_free();
        let due = self.pacer.due(
            elapsed,
            self.session.speed(),
            free,
            self.session.samples_per_frame(),
        );
        if due == 0 {
            return false;
        }
        for _ in 0..due {
            if let Err(error) = self.session.clock() {
                self.failure = Some(error.to_string());
                self.session.set_paused(true);
                return true;
            }
            self.samples.clear();
            audio::to_i16(
                self.session.samples(),
                audio::OUTPUT_GAIN,
                &mut self.samples,
            );
            let host = Rc::clone(&self.host);
            host.borrow_mut().write_audio(&self.samples);
        }
        self.frames_since_reading += due as u64;
        self.frames_since_save += due as u64;
        if !self.reported_running && self.session.frames() >= RUN_EVIDENCE_FRAMES {
            self.reported_running = true;
            self.reports.push(Report::Running {
                frames: self.session.frames(),
            });
        }
        if self.frames_since_save >= BATTERY_SAVE_FRAMES
            && self.session.info().is_some_and(|info| info.battery)
        {
            self.frames_since_save = 0;
            self.save_battery();
        }
        self.picture_dirty = true;
        true
    }

    /// One keyboard or focus event on the screen.
    fn event(&mut self, event: &Event) -> Option<Msg> {
        match event {
            Event::KeyDown {
                key,
                modifiers,
                repeat,
                ..
            } => {
                // Auto-repeat is not a new press; a shortcut key (Ctrl+...) is
                // the window's, never the pad's.
                if *repeat <= 1 {
                    if let Some(action) = input::action(*key, *modifiers) {
                        self.pad.key(action, true);
                    }
                }
                None
            }
            Event::KeyUp { key, modifiers, .. } => {
                if let Some(action) = input::action(*key, *modifiers) {
                    self.pad.key(action, false);
                }
                None
            }
            Event::KillFocus => {
                self.pad.release_all();
                None
            }
            _ => None,
        }
    }

    /// Paints the picture, the paused overlay and the FPS counter.
    fn paint(&mut self, canvas: &mut dyn Canvas) {
        let bounds = canvas.bounds();
        canvas.clear(Color::hex(0x00_00_00));
        if !self.session.is_loaded() {
            self.text_center(canvas, bounds, "No cartridge", 20.0, Color::hex(0x88_88_88));
            return;
        }
        let (x, y, width, height) = crate::video::layout(bounds, self.scale, self.aspect);
        let size = (width.max(1), height.max(1));
        if self.picture_dirty || self.image.is_none() || self.image_size() != size {
            let mut pixels = vec![0u8; (size.0 * size.1 * 4) as usize];
            self.scaler.resize(size.0, size.1);
            self.scaler.scale_into(self.session.frame(), &mut pixels);
            self.image = Image::from_rgba(size.0 as u32, size.1 as u32, pixels).ok();
            self.picture_dirty = false;
        }
        if let Some(image) = &self.image {
            canvas.draw_image(image, Rect::new(x, y, x + size.0, y + size.1));
        }
        if self.session.paused() {
            self.text_center(canvas, bounds, "Paused", 24.0, Color::hex(0xFF_D0_50));
        }
        if let Some(failure) = self.failure.clone() {
            self.text_center(canvas, bounds, &failure, 16.0, Color::hex(0xFF_60_60));
        }
        if self.show_fps {
            let style = TextStyle::new(Color::hex(0x9C_DC_60), Dip(12.0));
            canvas.draw_text(
                &format!("{} fps", self.fps()),
                Rect::new(
                    bounds.right - 70,
                    bounds.top + 4,
                    bounds.right - 4,
                    bounds.top + 20,
                ),
                &style,
            );
        }
    }

    /// Whether a picture has been scaled for the canvas (the tests use it to
    /// tell a painted control from one the layout never painted).
    pub fn has_image(&self) -> bool {
        self.image.is_some()
    }

    /// The picture's size, when there is one.
    pub fn image_size(&self) -> (i32, i32) {
        self.image
            .as_ref()
            .map(|image| (image.width() as i32, image.height() as i32))
            .unwrap_or_default()
    }

    /// A line of text centred in `bounds` (the overlays).
    fn text_center(
        &self,
        canvas: &mut dyn Canvas,
        bounds: Rect,
        text: &str,
        size: f32,
        color: Color,
    ) {
        let style = TextStyle::new(color, Dip(size)).centered().middle();
        let top = bounds.top + (bounds.height() - 40).max(0) / 2;
        canvas.draw_text(
            text,
            Rect::new(bounds.left, top, bounds.right, top + 40),
            &style,
        );
    }
}
