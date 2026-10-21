//! The picture: a custom control that shows the light view's picture, forwards the pointer and the
//! keyboard to it, and runs the demo's clock (`xui_webd_client::demo::Demo`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use xui_app::platform::webd::WebdTransport;
use xui_app::probe;
use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, WidgetId};
use xui_core::geometry::Size;
use xui_core::image::Image;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Canvas, Color, Dip, MouseButton, Rect, TextStyle};
use xui_webd_client::demo::Demo;
use xui_webd_client::page::{FIELD_BOX, LINK_BOX, TITLE};
use xui_webd_client::MouseKind;

use crate::keys::{char_keysym, keysym, modifier_bits};
use crate::Msg;

/// How often the view is polled for events (ms): a frame at 60 Hz.
const TICK_MS: u32 = 16;
/// xui's wheel unit: 120 for one notch.
const NOTCH: i32 = 120;

/// The control.
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
    pub fn new(ui: &Ui<Msg>) -> Screen {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )
        .expect("the web view control");
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
                let Ok(mut state) = state.try_borrow_mut() else {
                    return None;
                };
                state.event(&ui, id, event);
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
        Screen { control, state }
    }

    /// Ctrl+R: a new view now, whatever the old one was doing.
    pub fn recreate(&self) {
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.demo.recreate_now();
        }
    }
}

struct State {
    demo: Demo<WebdTransport>,
    started: Instant,
    /// The area the picture fills, as the last paint saw it.
    size: (u32, u32),
    /// The picture as an image, and how many frames had been applied when it was made (a new view
    /// counts its frames from 1 again, so the frame number alone cannot tell a new picture).
    image: Option<(u64, Image)>,
    /// Frames applied as of the last tick, to know when to draw again.
    shown: u64,
    /// The page's boxes were printed as probe rectangles (once the first picture is up).
    probed: bool,
    /// The button held since a press, for moves and the capture of the pointer.
    held: Option<MouseButton>,
    mods: u32,
}

impl State {
    fn new() -> State {
        State {
            demo: Demo::new(WebdTransport::new()),
            started: Instant::now(),
            size: (0, 0),
            image: None,
            shown: 0,
            probed: false,
            held: None,
            mods: 0,
        }
    }

    /// One turn of the demo's clock; whether the picture needs drawing.
    fn tick(&mut self) -> bool {
        let now_ms = self.started.elapsed().as_millis() as u64;
        for marker in self.demo.tick(self.size, now_ms) {
            trace::line!(Channel::Webddemo, "{}", marker.text());
        }
        let applied = self.demo.view().counters().applied;
        if applied > 0 && !self.probed {
            // Where the page put its link and its field, in the window's content: a session clicks
            // them by name (`{"click_at": {"window": "Web View Demo", "widget": "link"}}`).
            self.probed = true;
            for (name, (x, y, w, h)) in [("link", LINK_BOX), ("field", FIELD_BOX)] {
                probe::widget(TITLE, name, x, y, w, h);
            }
        }
        let changed = applied != self.shown;
        self.shown = applied;
        changed
    }

    fn paint(&mut self, canvas: &mut dyn Canvas) {
        let bounds = canvas.bounds();
        self.size = (bounds.width().max(0) as u32, bounds.height().max(0) as u32);
        canvas.clear(Color::hex(0xFF_FF_FF));
        let applied = self.demo.view().counters().applied;
        let picture = self.demo.picture();
        if picture.is_empty() {
            let style = TextStyle::new(Color::hex(0x55_55_55), Dip(14.0))
                .centered()
                .middle();
            canvas.draw_text("Starting the web view...", bounds, &style);
            return;
        }
        if self.image.as_ref().map(|(made_at, _)| *made_at) != Some(applied) {
            let (width, height) = (picture.width(), picture.height());
            self.image = Image::from_rgba(width, height, picture.rgba().to_vec())
                .ok()
                .map(|image| (applied, image));
        }
        if let Some((_, image)) = &self.image {
            // At its own size from the corner: while a resize is in flight it is never stretched.
            canvas.push_clip(bounds);
            canvas.draw_image(
                image,
                Rect::new(
                    bounds.left,
                    bounds.top,
                    bounds.left + image.width() as i32,
                    bounds.top + image.height() as i32,
                ),
            );
            canvas.pop_clip();
        }
    }

    fn event(&mut self, ui: &Ui<Msg>, id: WidgetId, event: &Event) {
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
                self.demo
                    .view()
                    .mouse(MouseKind::Press, button_number(button), x, y, self.mods);
            }
            Event::MouseUp {
                x,
                y,
                button,
                modifiers,
            } => {
                self.mods = modifier_bits(modifiers);
                self.demo
                    .view()
                    .mouse(MouseKind::Release, button_number(button), x, y, self.mods);
                if self.held == Some(button) {
                    self.held = None;
                    ui.release_capture();
                }
            }
            Event::MouseMove { x, y, modifiers } => {
                let held = self.held.map_or(0, button_number);
                self.demo
                    .view()
                    .mouse(MouseKind::Move, held, x, y, modifier_bits(modifiers));
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
                self.demo
                    .view()
                    .wheel(x, y, tx, ty, modifier_bits(modifiers));
            }
            Event::CaptureChanged => self.held = None,
            Event::SetFocus => self.demo.view().focus(true),
            Event::KillFocus => self.demo.view().focus(false),
            Event::KeyDown { key, modifiers, .. } => {
                self.mods = modifier_bits(modifiers);
                if let Some(sym) = keysym(key) {
                    self.demo.view().key(true, sym, "", self.mods);
                }
            }
            Event::KeyUp { key, modifiers, .. } => {
                self.mods = modifier_bits(modifiers);
                if let Some(sym) = keysym(key) {
                    self.demo.view().key(false, sym, "", self.mods);
                }
            }
            Event::Char(c) => {
                // Enter, Tab and Backspace came as keys; only text arrives here.
                if !c.is_control() {
                    let sym = char_keysym(c);
                    self.demo.view().key(true, sym, &c.to_string(), self.mods);
                    self.demo.view().key(false, sym, "", self.mods);
                }
            }
            _ => {}
        }
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
