//! A web view of an xui window that `webd` draws: [`LightView`] shows pages of HTML the app hands it
//! (docs/webd-plan.md, section 9.2: Docs and Mail), forwards the pointer and the keyboard to the page
//! and reports links and the first picture of each page.
//!
//! The page is WebKit's, in `webd`; this is only a picture (the newest frame, copied out of the shared
//! slot by [`xui_webd_client::Pane`]) and the plumbing. A light view loads nothing by itself: no
//! network, no storage, scripts off, and a click on a link comes back as [`LightEvent::Link`] for the
//! app to decide (a relative `.md` link in Docs, a `mailto:` or web link in Mail handed to `mimed`).
//! Copy (Ctrl+C) puts the page's selection on the clipboard. One view serves every page the window
//! shows: call [`LightView::set_html`] again, never make a view per page (docs/webd.md, "Limits").
//!
//! When `webd` is not running the view says so and keeps trying; the page appears once the service
//! is up. A view the service loses is made again and the newest page loaded into it.

pub mod keys;
mod wheel;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::Size;
use xui_core::image::Image;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Canvas, Color, Dip, MouseButton, Rect, TextStyle};
use xui_webd_client::{LightConfig, MouseKind, Pane, PaneEvent, Status};

use crate::platform::webd::WebdTransport;
use keys::{char_keysym, ctrl_letter, keysym, modifier_bits};
use wheel::{notches, wheel_steps};

/// How often the view is polled for events (ms): a frame at 60 Hz.
const TICK_MS: u32 = 16;
/// Height of the strip that says the engine is gone over a stale picture, in pixels.
const BANNER_H: i32 = 28;

/// What the view tells its app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LightEvent {
    /// A link was clicked, a form submitted or a meta refresh fired; nothing loaded. The URL is
    /// absolute against `about:blank` (so a relative link arrives as the text it has).
    Link(String),
    /// The page last given to [`LightView::set_html`] has been drawn for the first time.
    Painted,
    /// The view's status changed (the engine started, stopped, or gave up).
    Status(Status),
    /// webd refused the page last given to [`LightView::set_html`] (too large, say): the page on show is
    /// still the one before it, which the app must not leave as if it were the new one.
    LoadRefused(String),
}

/// The view.
pub struct LightView<M: 'static> {
    control: Control<M>,
    shared: Rc<RefCell<Shared>>,
}

struct Shared {
    pane: Pane<WebdTransport>,
    started: Instant,
    /// The area the picture fills, as the last paint saw it.
    size: (u32, u32),
    /// The picture as an image, and how many frames the pane had applied when it was made (a new
    /// view counts its frames from 1 again, so the picture's own number cannot tell a new image).
    image: Option<(u64, Image)>,
    /// Frames applied as of the last tick, to know when to draw again.
    shown: u64,
    /// The button held since a press, for moves and the capture of the pointer.
    held: Option<MouseButton>,
    mods: u32,
    background: Color,
    /// The newest pointer move and the wheel turns since the last tick, sent once per tick: a burst of
    /// either would overflow webd's inbox (a full inbox drops a oneway message), and the page only needs
    /// where the pointer is and how far the wheel went.
    pending: Pending,
}

/// Pointer input that waits for the next tick (see [`Shared::pending`]).
#[derive(Default)]
struct Pending {
    /// `(x, y, held button, modifiers)` of the newest move.
    motion: Option<(i32, i32, i32, u32)>,
    /// `(x, y, horizontal notches, vertical notches, modifiers)` summed since the last send.
    wheel: Option<(i32, i32, i32, i32, u32)>,
}

impl<M: 'static> LightView<M> {
    /// A view at `bounds` (device pixels; a layout may move it). `background` shows before the first
    /// picture and behind the status text; `on_event` maps what the page says to the app's messages
    /// (return `None` to ignore one). Fails only when the window cannot make a node.
    pub fn new(
        ui: &Ui<M>,
        bounds: Rect,
        background: Color,
        on_event: impl Fn(LightEvent) -> Option<M> + 'static,
    ) -> Result<LightView<M>> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, bounds).tab_stop())?;
        let mut config = LightConfig::default();
        config.user_style = zoom_css(ui.dpi());
        let shared = Rc::new(RefCell::new(Shared {
            pane: Pane::new(WebdTransport::new(), config),
            started: Instant::now(),
            size: (0, 0),
            image: None,
            shown: 0,
            held: None,
            mods: 0,
            background,
            pending: Pending::default(),
        }));
        {
            let shared = Rc::clone(&shared);
            control.set_painter(Rc::new(move |canvas| {
                if let Ok(mut shared) = shared.try_borrow_mut() {
                    shared.paint(canvas);
                }
            }));
        }
        {
            let (shared, ui, id) = (Rc::clone(&shared), ui.clone(), control.id());
            control.on_events(move |event| {
                let Ok(mut shared) = shared.try_borrow_mut() else {
                    return None;
                };
                shared.event(&ui, id, event);
                None
            });
        }
        {
            let (shared, ui, id) = (Rc::clone(&shared), ui.clone(), control.id());
            control.set_timer(TICK_MS, move || {
                let mut shared = shared.try_borrow_mut().ok()?;
                let (dirty, events) = shared.tick(&ui);
                if dirty {
                    ui.invalidate(id);
                }
                let mut messages = events.into_iter().filter_map(&on_event);
                // The timer returns one message; the rest join the queue behind it.
                let first = messages.next();
                for message in messages {
                    ui.emit(message);
                }
                first
            });
        }
        Ok(LightView { control, shared })
    }

    /// The view's node, for a layout to place.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Moves and resizes the view; the page is laid out at the new size on the next turn.
    pub fn set_bounds(&self, rect: Rect) {
        self.control.set_bounds(rect);
    }

    /// Shows `html` in place of the page on show (at `about:blank`: nothing it names is loaded).
    pub fn set_html(&self, html: String) {
        if let Ok(mut shared) = self.shared.try_borrow_mut() {
            shared.pane.set_html(html);
        }
    }

    /// The colour behind the status text and before the first picture.
    pub fn set_background(&self, background: Color) {
        if let Ok(mut shared) = self.shared.try_borrow_mut() {
            shared.background = background;
        }
        self.control.invalidate();
    }

    /// What the view can show now.
    pub fn status(&self) -> Status {
        self.shared
            .try_borrow()
            .map_or(Status::Starting, |shared| shared.pane.status().clone())
    }

    /// Try again at once (a key after a failure).
    pub fn retry(&self) {
        if let Ok(mut shared) = self.shared.try_borrow_mut() {
            shared.pane.retry();
        }
    }

    /// Gives the view the keyboard focus.
    pub fn focus(&self) {
        self.control.focus();
    }
}

impl<M: 'static> Placeable<M> for LightView<M> {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<M>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

/// The style that makes a page as big as the window's scale: `webd`'s light view has no scale of its
/// own, and CSS `zoom` on the root scales the layout while the pointer stays in view pixels.
fn zoom_css(dpi: u32) -> String {
    if dpi < 120 {
        return String::new();
    }
    format!("html {{ zoom: {:.2}; }}", dpi as f32 / 96.0)
}

impl Shared {
    /// One turn of the pane's clock; whether the picture needs drawing, and what the app is told.
    fn tick<M: 'static>(&mut self, ui: &Ui<M>) -> (bool, Vec<LightEvent>) {
        self.flush_pointer();
        let now_ms = self.started.elapsed().as_millis() as u64;
        let mut told = Vec::new();
        let mut dirty = false;
        for event in self.pane.tick(self.size, now_ms) {
            match event {
                PaneEvent::Link(url) => told.push(LightEvent::Link(url)),
                PaneEvent::Painted => told.push(LightEvent::Painted),
                PaneEvent::Copy(text) => ui.set_clipboard_text(&text),
                PaneEvent::Status(status) => {
                    dirty = true;
                    told.push(LightEvent::Status(status));
                }
                PaneEvent::LoadRefused(why) => {
                    dirty = true;
                    told.push(LightEvent::LoadRefused(why));
                }
            }
        }
        let applied = self.pane.applied();
        let changed = applied != self.shown || dirty;
        self.shown = applied;
        (changed, told)
    }

    /// Sends the pointer move and the wheel turns that waited for this tick.
    fn flush_pointer(&mut self) {
        if let Some((x, y, button, mods)) = self.pending.motion.take() {
            self.pane.view().mouse(MouseKind::Move, button, x, y, mods);
        }
        if let Some((x, y, across, down, mods)) = self.pending.wheel.take() {
            for (step_x, step_y) in wheel_steps(across, down) {
                self.pane.view().wheel(x, y, step_x, step_y, mods);
            }
        }
    }

    fn paint(&mut self, canvas: &mut dyn Canvas) {
        let bounds = canvas.bounds();
        self.size = (bounds.width().max(0) as u32, bounds.height().max(0) as u32);
        canvas.clear(self.background);
        let applied = self.pane.applied();
        let picture = self.pane.picture();
        if !picture.is_empty() {
            if self.image.as_ref().map(|(made_at, _)| *made_at) != Some(applied) {
                self.image =
                    Image::from_rgba(picture.width(), picture.height(), picture.rgba().to_vec())
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
        self.paint_status(canvas, bounds);
    }

    /// What the person should know when the engine is not there: centred when there is no picture,
    /// a strip across the top over a stale one.
    fn paint_status(&self, canvas: &mut dyn Canvas, bounds: Rect) {
        let (headline, detail) = match self.pane.status() {
            Status::Starting | Status::Live => return,
            Status::NotRunning => (
                "The web engine is not running".to_owned(),
                "Log out and in again, or start \"Web Engine\" from the applications menu."
                    .to_owned(),
            ),
            Status::Failed(why) => (
                "The web engine stopped".to_owned(),
                format!("{why}. Press any key to try again."),
            ),
        };
        let ink = readable_on(self.background);
        if self.pane.picture().is_empty() {
            let middle = (bounds.top + bounds.bottom) / 2;
            let head = Rect::new(bounds.left, middle - 30, bounds.right, middle - 4);
            let tail = Rect::new(bounds.left, middle + 4, bounds.right, middle + 28);
            canvas.draw_text(
                &headline,
                head,
                &TextStyle::new(ink, Dip(16.0)).centered().middle(),
            );
            canvas.draw_text(
                &detail,
                tail,
                &TextStyle::new(ink, Dip(13.0)).centered().middle(),
            );
        } else {
            let strip = Rect::new(bounds.left, bounds.top, bounds.right, bounds.top + BANNER_H);
            canvas.fill_rect(strip, self.background);
            let text = format!("{headline}. {detail}");
            canvas.draw_text(
                &text,
                strip,
                &TextStyle::new(ink, Dip(13.0)).centered().middle(),
            );
        }
    }

    fn event<M: 'static>(&mut self, ui: &Ui<M>, id: WidgetId, event: &Event) {
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
                // A move that waits for the tick goes first: the press is where the pointer is.
                self.flush_pointer();
                self.mods = modifier_bits(modifiers);
                ui.focus(id);
                if self.held.is_none() {
                    self.held = Some(button);
                    ui.set_capture(id);
                }
                self.pane
                    .view()
                    .mouse(MouseKind::Press, button_number(button), x, y, self.mods);
            }
            Event::MouseUp {
                x,
                y,
                button,
                modifiers,
            } => {
                self.flush_pointer();
                self.mods = modifier_bits(modifiers);
                self.pane
                    .view()
                    .mouse(MouseKind::Release, button_number(button), x, y, self.mods);
                if self.held == Some(button) {
                    self.held = None;
                    ui.release_capture();
                }
            }
            Event::MouseMove { x, y, modifiers } => {
                let held = self.held.map_or(0, button_number);
                self.pending.motion = Some((x, y, held, modifier_bits(modifiers)));
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
                let (_, _, across, down, _) = self.pending.wheel.unwrap_or_default();
                let sum = |a: i32, b: i32| a.saturating_add(b);
                self.pending.wheel = Some((
                    x,
                    y,
                    sum(across, tx),
                    sum(down, ty),
                    modifier_bits(modifiers),
                ));
            }
            Event::CaptureChanged => self.held = None,
            Event::SetFocus => self.pane.view().focus(true),
            Event::KillFocus => self.pane.view().focus(false),
            Event::KeyDown { key, modifiers, .. } => {
                self.mods = modifier_bits(modifiers);
                // A key after a failure is the way to try again (there is no button to press).
                if matches!(self.pane.status(), Status::Failed(_)) {
                    self.pane.retry();
                }
                let sym =
                    keysym(key).or_else(|| modifiers.ctrl.then(|| ctrl_letter(key)).flatten());
                if let Some(sym) = sym {
                    self.pane.view().key(true, sym, "", self.mods);
                }
            }
            Event::KeyUp { key, modifiers, .. } => {
                self.mods = modifier_bits(modifiers);
                let sym =
                    keysym(key).or_else(|| modifiers.ctrl.then(|| ctrl_letter(key)).flatten());
                if let Some(sym) = sym {
                    self.pane.view().key(false, sym, "", self.mods);
                }
            }
            Event::Char(c) => {
                // Enter, Tab and Backspace came as keys; only text arrives here.
                if !c.is_control() {
                    let sym = char_keysym(c);
                    self.pane.view().key(true, sym, &c.to_string(), self.mods);
                    self.pane.view().key(false, sym, "", self.mods);
                }
            }
            _ => {}
        }
    }
}

/// Black or white, whichever reads on `background`.
fn readable_on(background: Color) -> Color {
    let luminance = u32::from(background.r) * 299
        + u32::from(background.g) * 587
        + u32::from(background.b) * 114;
    if luminance > 128_000 {
        Color::hex(0x20_20_20)
    } else {
        Color::hex(0xe8_e8_e8)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_zoomed_only_on_a_scaled_display() {
        assert_eq!(zoom_css(96), "");
        assert_eq!(zoom_css(192), "html { zoom: 2.00; }");
        assert_eq!(zoom_css(144), "html { zoom: 1.50; }");
    }

    #[test]
    fn text_is_dark_on_light_and_light_on_dark() {
        assert_eq!(readable_on(Color::hex(0xff_ff_ff)), Color::hex(0x20_20_20));
        assert_eq!(readable_on(Color::hex(0x23_2a_40)), Color::hex(0xe8_e8_e8));
    }
}
