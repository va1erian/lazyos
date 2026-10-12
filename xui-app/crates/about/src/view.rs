//! The About widget: one custom-painted xui node that fills the window. A
//! repeating timer renders the scene into an image at an internal
//! resolution that adapts to the frame cost; the painter blits it, then
//! draws the translucent text panel over it with the canvas's own text.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::{Key, MouseButton};
use xui_core::widget::{Control, Placeable};
use xui_core::Image;

use crate::app::{Info, Msg, Report};
use crate::pages::PAGES;
use crate::panel::{layout, paint};
use crate::raster::Framebuffer;
use crate::scene::{Area, Scene};

/// The timer period: fast enough for 60 frames a second; a slow frame
/// simply delays the next tick.
const TICK_MS: u32 = 15;
/// Seconds a page stays up before the next one, and the pause a key press
/// buys.
const PAGE_SECONDS: f32 = 9.0;
const PAGE_HOLD_SECONDS: f32 = 24.0;
/// A frame that costs more than this (ms) coarsens the internal resolution.
const SLOW_MS: f32 = 38.0;
const WHEEL_NOTCH: f32 = 120.0;

/// Everything the widget changes, shared by the painter, the timer and the
/// event mapper (never borrowed across a call back into xui).
pub struct State {
    pub scene: Scene,
    pub(crate) page: usize,
    page_clock: f32,
    divisor: usize,
    pub(crate) image: Option<Image>,
    size: (usize, usize),
    last: Option<Instant>,
    second_start: Option<Instant>,
    frames: u32,
    work_ms: f32,
    fps: u32,
    reports: Vec<Report>,
    pub(crate) info: Info,
}

impl State {
    fn new(info: Info) -> State {
        State {
            scene: Scene::new(),
            page: 0,
            page_clock: 0.0,
            divisor: 2,
            image: None,
            size: (0, 0),
            last: None,
            second_start: None,
            frames: 0,
            work_ms: 0.0,
            fps: 0,
            reports: Vec::new(),
            info,
        }
    }

    /// Advances time to `now` and renders a frame for a `w` x `h` window
    /// (physical pixels at `scale` times the design size). Returns whether
    /// there is a new image.
    pub fn tick(&mut self, w: usize, h: usize, scale: i32, now: Instant) -> bool {
        if w < 8 || h < 8 {
            return false;
        }
        let dt = self
            .last
            .map_or(0.016, |t| now.duration_since(t).as_secs_f32());
        self.last = Some(now);
        self.scene.advance(dt);
        self.page_clock += dt;
        if self.page_clock > PAGE_SECONDS {
            self.set_page((self.page + 1) % PAGES.len(), false);
        }

        let started = Instant::now();
        let lay = layout(w as i32, h as i32, scale);
        let d = self.divisor as f32;
        let (fw, fh) = ((w / self.divisor).max(1), (h / self.divisor).max(1));
        let area = Area {
            x0: lay.area.0 as f32 / d,
            y0: lay.area.1 as f32 / d,
            x1: lay.area.2 as f32 / d,
            y1: lay.area.3 as f32 / d,
        };
        // The last image's buffer is filled again in place: a window-sized
        // buffer is large enough to be an `mmap`, with a page fault per page
        // each frame otherwise.
        let pixels = self
            .image
            .take()
            .map(Image::into_pixels)
            .unwrap_or_default();
        let mut fb = Framebuffer::new(fw, fh, pixels);
        self.scene.render(&mut fb, area);
        self.image = Image::from_rgba(fw as u32, fh as u32, fb.rgba).ok();
        self.size = (fw, fh);

        self.account(started.elapsed().as_secs_f32() * 1000.0, now);
        true
    }

    /// Frame statistics once a second, and the resolution that keeps them
    /// healthy.
    fn account(&mut self, work_ms: f32, now: Instant) {
        self.frames += 1;
        self.work_ms = if self.work_ms == 0.0 {
            work_ms
        } else {
            self.work_ms * 0.85 + work_ms * 0.15
        };
        let start = *self.second_start.get_or_insert(now);
        if now.duration_since(start) < Duration::from_secs(1) {
            return;
        }
        self.fps = self.frames;
        self.frames = 0;
        self.second_start = Some(now);
        let finer = {
            let ratio = self.divisor as f32 / (self.divisor as f32 - 1.0).max(1.0);
            self.divisor > 1 && self.work_ms * ratio * ratio < SLOW_MS * 0.6
        };
        if self.work_ms > SLOW_MS && self.divisor < 6 {
            self.divisor += 1;
        } else if finer && self.fps >= 30 {
            self.divisor -= 1;
        }
        self.reports.push(Report::Frame {
            fps: self.fps,
            width: self.size.0,
            height: self.size.1,
            divisor: self.divisor,
            millis: self.work_ms,
            triangles: self.scene.triangles(),
        });
    }

    fn set_page(&mut self, page: usize, by_user: bool) {
        self.page = page;
        self.page_clock = if by_user {
            PAGE_SECONDS - PAGE_HOLD_SECONDS
        } else {
            0.0
        };
        self.reports.push(Report::Page(page));
    }

    /// The oldest report not yet heard.
    pub fn take_report(&mut self) -> Option<Report> {
        (!self.reports.is_empty()).then(|| self.reports.remove(0))
    }

    /// The text under the model: what is drawn and how fast.
    pub(crate) fn status(&self) -> String {
        format!(
            "{}  \u{b7}  {} triangles  \u{b7}  {}  \u{b7}  {} fps  \u{b7}  {}x{}",
            self.scene.shape().name(),
            self.scene.triangles(),
            self.scene.style.name(),
            self.fps,
            self.size.0 * self.divisor,
            self.size.1 * self.divisor,
        )
    }
}

pub struct AboutView {
    control: Control<Msg>,
    state: Rc<RefCell<State>>,
}

impl AboutView {
    pub fn new(ui: &Ui<Msg>, info: Info) -> Result<AboutView> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state = Rc::new(RefCell::new(State::new(info)));
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(state) = state.try_borrow() {
                    paint(canvas, &state);
                }
            }));
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            let drag = Rc::new(Cell::new(None));
            control.on_events(move |event| handle(&ui, id, &state, &drag, event));
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            control.set_timer(TICK_MS, move || {
                let bounds = ui.bounds(id);
                let scale = (ui.dpi() as i32 / 96).max(1);
                let (changed, report) = {
                    let mut state = state.try_borrow_mut().ok()?;
                    let changed = state.tick(
                        bounds.width().max(0) as usize,
                        bounds.height().max(0) as usize,
                        scale,
                        Instant::now(),
                    );
                    (changed, state.take_report())
                };
                // The borrow ends first: a backend may paint from inside
                // `invalidate`.
                if changed {
                    ui.invalidate(id);
                }
                report.map(Msg::Report)
            });
        }
        Ok(AboutView { control, state })
    }

    pub fn focus(&self) {
        self.control.focus();
    }

    /// The state, for a host or a test to drive and inspect.
    pub fn state(&self) -> Rc<RefCell<State>> {
        Rc::clone(&self.state)
    }
}

impl Placeable<Msg> for AboutView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

type Drag = Cell<Option<(i32, i32)>>;

/// The event mapper. Calls back into xui happen after the state is let go,
/// because the winit backend delivers events from inside them.
fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &RefCell<State>,
    drag: &Drag,
    event: &Event,
) -> Option<Msg> {
    let mut focus = false;
    let mut capture = false;
    let mut release = false;
    let msg = {
        let Ok(mut st) = state.try_borrow_mut() else {
            return None;
        };
        match *event {
            Event::KeyDown { key, repeat, .. } => key_down(&mut st, key, repeat),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => {
                focus = true;
                capture = true;
                drag.set(Some((x, y)));
                st.scene.grab();
                None
            }
            Event::MouseMove { x, y, .. } => {
                if let Some((px, py)) = drag.get() {
                    st.scene.drag((x - px) as f32, (y - py) as f32);
                    drag.set(Some((x, y)));
                }
                None
            }
            Event::MouseUp { .. } => {
                if drag.take().is_some() {
                    st.scene.release();
                }
                release = true;
                None
            }
            Event::CaptureChanged | Event::KillFocus => {
                if drag.take().is_some() {
                    st.scene.release();
                }
                None
            }
            Event::MouseWheel {
                delta,
                horizontal: false,
                ..
            } => {
                st.scene.zoom(f32::from(delta) / WHEEL_NOTCH);
                None
            }
            _ => None,
        }
    };
    if focus {
        ui.focus(id);
    }
    if capture {
        ui.set_capture(id);
    }
    if release {
        ui.release_capture();
    }
    msg
}

fn key_down(st: &mut State, key: Key, repeat: u16) -> Option<Msg> {
    if repeat > 1 {
        return None;
    }
    let pages = PAGES.len();
    match key {
        k if k == Key::ESCAPE || k == Key::Q => return Some(Msg::Quit),
        k if k == Key::RIGHT || k == Key::PAGE_DOWN => st.set_page((st.page + 1) % pages, true),
        k if k == Key::LEFT || k == Key::PAGE_UP => {
            st.set_page((st.page + pages - 1) % pages, true)
        }
        k if k == Key::SPACE || k == Key::TAB || k == Key::S => {
            let shape = st.scene.shape().next();
            st.scene.set_shape(shape);
            st.reports.push(Report::Shape(shape.name()));
        }
        k if k == Key::W => {
            st.scene.style = st.scene.style.next();
            st.reports.push(Report::Style(st.scene.style.name()));
        }
        k if k == Key::UP => st.scene.zoom(1.0),
        k if k == Key::DOWN => st.scene.zoom(-1.0),
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_advance_on_their_own_and_a_key_holds_them() {
        let mut st = State::new(Info {
            version: "0".into(),
        });
        let t0 = Instant::now();
        assert!(st.tick(64, 40, 1, t0));
        for i in 1..=10 {
            st.tick(64, 40, 1, t0 + Duration::from_secs(i));
        }
        assert!(st.page > 0, "no page change in 10 s");
        let before = st.page;
        key_down(&mut st, Key::RIGHT, 1);
        assert_eq!(st.page, (before + 1) % PAGES.len());
        let held = st.page;
        for i in 11..=20 {
            st.tick(64, 40, 1, t0 + Duration::from_secs(i));
        }
        assert_eq!(st.page, held, "a key press should hold the page");
    }
}
