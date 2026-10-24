//! The window's one control: view A's picture over a status line with the last step, input
//! forwarded to view A, and the scenario's clock (`xui_webd_client::browse::Browse`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use xui_app::lightview::keys::{char_keysym, keysym, modifier_bits};
use xui_app::platform::webd::WebdTransport;
use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, WidgetId};
use xui_core::geometry::Size;
use xui_core::image::Image;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Canvas, Color, Dip, MouseButton, Rect, TextStyle};
use xui_webd_client::browse::{base_url, Browse, Marker};
use xui_webd_client::MouseKind;

use crate::Msg;

/// How often the views are polled (ms): the latency step's resolution.
const TICK_MS: u32 = 8;
/// The status line under the picture, in pixels.
const STATUS: i32 = 22;
/// xui's wheel unit: 120 for one notch.
const NOTCH: i32 = 120;

pub struct Screen {
    control: Control<Msg>,
    _state: Rc<RefCell<State>>,
}

impl Placeable<Msg> for Screen {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

impl Screen {
    pub fn new(ui: &Ui<Msg>) -> Screen {
        let spec = NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop();
        let control = Control::new(ui, &spec).expect("the web view control");
        let state = Rc::new(RefCell::new(State::new()));
        {
            let state = Rc::clone(&state);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.paint(canvas);
                }
            }));
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            control.on_events(move |event| {
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.event(&ui, id, event);
                }
                None
            });
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            control.set_timer(TICK_MS, move || {
                let mut state = state.try_borrow_mut().ok()?;
                if state.tick() {
                    ui.invalidate(id);
                }
                None
            });
        }
        control.focus();
        Screen {
            control,
            _state: state,
        }
    }
}

struct State {
    browse: Browse<WebdTransport>,
    started: Instant,
    /// The area view A fills, as the last paint saw it.
    size: (u32, u32),
    /// The picture as an image, and the frame number it was made from.
    image: Option<(u64, Image)>,
    /// The frame number on show at the last tick.
    shown: u64,
    status: String,
    held: Option<MouseButton>,
    mods: u32,
}

impl State {
    fn new() -> State {
        let base = base_url(std::env::args().skip(1));
        State {
            browse: Browse::new(WebdTransport::full, &base),
            started: Instant::now(),
            size: (0, 0),
            image: None,
            shown: 0,
            status: format!("Waiting for webd ({base})"),
            held: None,
            mods: 0,
        }
    }

    /// One turn of the scenario; whether the window needs drawing.
    fn tick(&mut self) -> bool {
        let now_ms = self.started.elapsed().as_millis() as u64;
        let markers = self.browse.tick(self.size, now_ms);
        let said = !markers.is_empty();
        for marker in markers {
            report(&marker);
            self.status = marker.text();
        }
        let seq = self.browse.picture().map_or(0, |p| p.seq());
        let changed = seq != self.shown;
        self.shown = seq;
        changed || said
    }

    fn paint(&mut self, canvas: &mut dyn Canvas) {
        let bounds = canvas.bounds();
        let view = Rect::new(
            bounds.left,
            bounds.top,
            bounds.right,
            (bounds.bottom - STATUS).max(bounds.top),
        );
        self.size = (view.width().max(0) as u32, view.height().max(0) as u32);
        canvas.clear(Color::hex(0xFF_FF_FF));
        self.paint_picture(canvas, view);
        let strip = Rect::new(bounds.left, view.bottom, bounds.right, bounds.bottom);
        canvas.fill_rect(strip, Color::hex(0x20_24_2C));
        let style = TextStyle::new(Color::hex(0xE8_EA_ED), Dip(12.0)).middle();
        canvas.draw_text(&self.status, strip.shrink(4), &style);
    }

    fn paint_picture(&mut self, canvas: &mut dyn Canvas, view: Rect) {
        let Some(picture) = self.browse.picture().filter(|p| !p.is_empty()) else {
            let style = TextStyle::new(Color::hex(0x55_55_55), Dip(14.0))
                .centered()
                .middle();
            canvas.draw_text("Starting the web view...", view, &style);
            return;
        };
        let seq = picture.seq();
        if self.image.as_ref().map(|(made, _)| *made) != Some(seq) {
            let (width, height) = (picture.width(), picture.height());
            self.image = Image::from_rgba(width, height, picture.rgba().to_vec())
                .ok()
                .map(|image| (seq, image));
        }
        if let Some((_, image)) = &self.image {
            // At its own size from the corner: never stretched while a resize is in flight.
            let at = Rect::new(
                view.left,
                view.top,
                view.left + image.width() as i32,
                view.top + image.height() as i32,
            );
            canvas.push_clip(view);
            canvas.draw_image(image, at);
            canvas.pop_clip();
        }
    }

    fn event(&mut self, ui: &Ui<Msg>, id: WidgetId, event: &Event) {
        let Some(view) = self.browse.view() else {
            return;
        };
        match *event {
            Event::MouseDown {
                x,
                y,
                button,
                modifiers,
            }
            | Event::MouseDoubleClick {
                x,
                y,
                button,
                modifiers,
            } => {
                self.mods = modifier_bits(modifiers);
                ui.focus(id);
                if self.held.is_none() {
                    self.held = Some(button);
                    ui.set_capture(id);
                }
                view.mouse(MouseKind::Press, button_number(button), x, y, self.mods);
            }
            Event::MouseUp {
                x,
                y,
                button,
                modifiers,
            } => {
                self.mods = modifier_bits(modifiers);
                view.mouse(MouseKind::Release, button_number(button), x, y, self.mods);
                if self.held == Some(button) {
                    self.held = None;
                    ui.release_capture();
                }
            }
            Event::MouseMove { x, y, modifiers } => {
                let held = self.held.map_or(0, button_number);
                view.mouse(MouseKind::Move, held, x, y, modifier_bits(modifiers));
            }
            Event::MouseWheel {
                delta,
                horizontal,
                x,
                y,
                modifiers,
            } => {
                let ticks = notches(delta);
                let (tx, ty) = if horizontal { (ticks, 0) } else { (0, ticks) };
                // This view shows no picture ahead of the page: it asks for no answers (0).
                view.wheel(x, y, tx, ty, modifier_bits(modifiers), 0);
            }
            Event::CaptureChanged => self.held = None,
            Event::SetFocus => view.focus(true),
            Event::KillFocus => view.focus(false),
            Event::KeyDown { key, modifiers, .. } => {
                self.mods = modifier_bits(modifiers);
                if let Some(sym) = keysym(key) {
                    view.key(true, sym, "", self.mods);
                }
            }
            Event::KeyUp { key, modifiers, .. } => {
                self.mods = modifier_bits(modifiers);
                if let Some(sym) = keysym(key) {
                    view.key(false, sym, "", self.mods);
                }
            }
            Event::Char(c) if !c.is_control() => {
                let sym = char_keysym(c);
                view.key(true, sym, &c.to_string(), self.mods);
                view.key(false, sym, "", self.mods);
            }
            _ => {}
        }
    }
}

/// One step's evidence line: `WEBDBROWSE:<STEP>:PASS[:<detail>]` or `...:FAIL:<why>`.
fn report(marker: &Marker) {
    match (marker.pass, marker.detail.is_empty()) {
        (true, true) => trace::pass!(Channel::Webdbrowse, marker.step),
        (true, false) => trace::pass!(Channel::Webdbrowse, marker.step, "{}", marker.detail),
        (false, _) => trace::fail!(Channel::Webdbrowse, marker.step, "{}", marker.detail),
    }
}

fn button_number(button: MouseButton) -> i32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::X1 | MouseButton::X2 => 0,
    }
}

/// Whole notches of a wheel `delta`; a small (smooth) delta still moves once.
fn notches(delta: i16) -> i32 {
    let delta = i32::from(delta);
    match delta / NOTCH {
        0 => delta.signum(),
        whole => whole,
    }
}
