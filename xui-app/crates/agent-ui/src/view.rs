//! TranscriptView (docs/ai-agent-plan.md §9.1): one custom-painted control
//! that draws the conversation. It is virtualized (paint is O(visible
//! rows)), scrolls by anchor and never shapes text itself: rows come in
//! prepared. Modelled on LazySky's FeedView.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Canvas, Corner, Event, NodeKind, NodeSpec, Result, Rgba, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::{Key, MouseButton};
use xui_core::widget::{Control, Placeable};
use xui_core::Theme;

use crate::model::{RowId, Transcript};
use crate::prepare::{Fill, Piece, Prepared, Target, Tone};
use crate::Msg;

/// One wheel notch, in pixels at 96 dpi.
pub const WHEEL_96: i32 = 48;
/// The scroll bar's thickness at 96 dpi.
pub const BAR_96: i32 = 10;
/// The `MouseWheel` delta of one notch.
const WHEEL_DELTA: i32 = 120;

/// What a click in the transcript means; the app decides what to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewAction {
    Link(String),
    Copy(String),
    Toggle(RowId),
}

#[derive(Clone, Copy)]
struct Grab {
    from: i32,
    scroll: i32,
}

pub struct ViewState {
    pub transcript: Transcript<Prepared>,
    pub dpi: u32,
    /// The view in device pixels.
    pub width: i32,
    pub height: i32,
    /// The width changed: rows need laying out again.
    pub relayout: bool,
    /// The position changed: more rows may need shaping.
    pub moved: bool,
    grab: Option<Grab>,
    /// A note shown over an empty transcript.
    pub note: String,
}

impl ViewState {
    pub fn new() -> ViewState {
        ViewState {
            transcript: Transcript::new(),
            dpi: 96,
            width: 0,
            height: 0,
            relayout: false,
            moved: false,
            grab: None,
            note: String::new(),
        }
    }

    pub fn scaled(&self, design: i32) -> i32 {
        (design * self.dpi.max(1) as i32 + 48) / 96
    }

    /// The width rows are laid out at: the view less the scroll bar.
    pub fn content_width(&self) -> i32 {
        (self.width - self.scaled(BAR_96)).max(1)
    }

    pub fn set_view(&mut self, width: i32, height: i32, dpi: u32) {
        if width != self.width || dpi != self.dpi {
            self.relayout = true;
        }
        self.width = width;
        self.height = height;
        self.dpi = dpi;
        self.transcript.set_view_height(height);
        self.moved = true;
    }

    /// The thumb of the scroll bar in view-local pixels: `(top, length)`.
    pub fn thumb(&self) -> Option<(i32, i32)> {
        let (total, view) = (self.transcript.total_height(), self.height);
        if total <= view || view <= 0 {
            return None;
        }
        let min = self.scaled(BAR_96) * 2;
        let len = ((view as i64 * view as i64) / total as i64).max(min as i64) as i32;
        let len = len.min(view);
        let room = (total - view).max(1);
        let at = (self.transcript.scroll_top() as i64 * (view - len) as i64 / room as i64) as i32;
        Some((at, len))
    }
}

impl Default for ViewState {
    fn default() -> ViewState {
        ViewState::new()
    }
}

pub struct TranscriptView {
    control: Control<Msg>,
    ui: Ui<Msg>,
    pub state: Rc<RefCell<ViewState>>,
}

impl TranscriptView {
    pub fn new(ui: &Ui<Msg>) -> Result<TranscriptView> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state = Rc::new(RefCell::new(ViewState::new()));
        {
            let (state, theme) = (Rc::clone(&state), ui.theme_handle());
            control.set_painter(Rc::new(move |canvas| {
                // Never borrowed across a call back into xui: a paint from
                // inside an event just catches up on the next frame.
                if let Ok(state) = state.try_borrow() {
                    paint(canvas, &state, &theme.get());
                }
            }));
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            control.on_events(move |event| handle(&ui, id, &state, event));
        }
        Ok(TranscriptView {
            control,
            ui: ui.clone(),
            state,
        })
    }

    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    pub fn invalidate(&self) {
        self.ui.invalidate(self.control.id());
    }

    pub fn focus(&self) {
        self.control.focus();
    }
}

impl Placeable<Msg> for TranscriptView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        let dpi = self.control.dpi();
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.set_view(rect.width(), rect.height(), dpi);
        }
        self.invalidate();
    }
}

fn tone_colour(theme: &Theme, tone: Tone) -> Rgba {
    match tone {
        Tone::Primary => Rgba::from(theme.text),
        Tone::Secondary => Rgba::from(theme.text_secondary),
        Tone::Accent => Rgba::from(theme.accent),
        Tone::Danger => Rgba::with_alpha(0xC0, 0x39, 0x2B, 255),
    }
}

fn fill_colour(theme: &Theme, fill: Fill) -> Rgba {
    let a = theme.accent;
    match fill {
        Fill::User => Rgba::with_alpha(a.r, a.g, a.b, 36),
        Fill::Code => Rgba::with_alpha(theme.text.r, theme.text.g, theme.text.b, 20),
        Fill::Failure => Rgba::with_alpha(0xC0, 0x39, 0x2B, 40),
        Fill::Button => Rgba::with_alpha(a.r, a.g, a.b, 40),
    }
}

/// Paints the visible rows of `state`, a scroll bar over them. Only the rows
/// that stick out of the view are drawn under a clip (a clip makes every
/// draw apply a mask).
pub fn paint(canvas: &mut dyn Canvas, state: &ViewState, theme: &Theme) {
    let bounds = canvas.bounds();
    canvas.clear(theme.background);
    let t = &state.transcript;
    for i in t.visible() {
        let Some(row) = t.row(i) else { continue };
        let top = bounds.top + t.screen_top(i);
        let rect = Rect::new(
            bounds.left,
            top,
            bounds.left + state.width,
            top + row.height,
        );
        let clipped = rect.top < bounds.top || rect.bottom > bounds.bottom;
        if clipped {
            canvas.push_clip(within(rect, bounds));
        }
        match &row.prepared {
            Some(prepared) => draw_row(canvas, prepared, (rect.left, rect.top), state.width, theme),
            None => skeleton(canvas, rect, state, theme),
        }
        if clipped {
            canvas.pop_clip();
        }
    }
    if t.is_empty() && !state.note.is_empty() {
        let mut style = xui_core::TextStyle::new(theme.text_secondary, xui_core::Dip(14.0));
        style.align = xui_core::backend::TextAlign::Center;
        style.valign = xui_core::backend::TextVAlign::Middle;
        style.wrap = true;
        canvas.draw_text(&state.note, bounds, &style);
    }
    scroll_bar(canvas, bounds, state, theme);
}

fn draw_row(canvas: &mut dyn Canvas, p: &Prepared, origin: (i32, i32), width: i32, theme: &Theme) {
    let shift = |r: &Rect| {
        Rect::new(
            origin.0 + r.left,
            origin.1 + r.top,
            origin.0 + r.right,
            origin.1 + r.bottom,
        )
    };
    // Fills and tints first, so the text draws over them.
    for piece in &p.pieces {
        match piece {
            Piece::Fill { rect, fill } => {
                canvas.fill_rounded_rect_corners(
                    shift(rect),
                    [Corner::uniform(6.0); 4],
                    fill_colour(theme, *fill),
                );
            }
            Piece::LinkTint { rect } => {
                let a = theme.accent;
                canvas.fill_rect_rgba(shift(rect), Rgba::with_alpha(a.r, a.g, a.b, 48));
            }
            Piece::Bar { rect } => {
                canvas.fill_rect_rgba(shift(rect), Rgba::from(theme.border));
            }
            _ => {}
        }
    }
    for piece in &p.pieces {
        match piece {
            Piece::Text { layout, at, tone } => {
                canvas.draw_layout(
                    &**layout,
                    Point::new(origin.0 + at.0, origin.1 + at.1),
                    tone_colour(theme, *tone),
                );
            }
            Piece::Rule { y } => {
                let y = origin.1 + y;
                canvas.draw_line(
                    Point::new(origin.0, y),
                    Point::new(origin.0 + width, y),
                    theme.border,
                    1.0,
                );
            }
            _ => {}
        }
    }
}

/// A row not yet shaped: flat bars at its estimated height.
fn skeleton(canvas: &mut dyn Canvas, rect: Rect, state: &ViewState, theme: &Theme) {
    let pad = state.scaled(14);
    let bar = state.scaled(10);
    let right = rect.right - pad - state.scaled(BAR_96);
    let mut y = rect.top + pad;
    for frac in [30, 90, 70] {
        let w = ((right - rect.left - pad).max(0) * frac) / 100;
        let r = Rect::new(rect.left + pad, y, rect.left + pad + w, y + bar);
        if r.bottom < rect.bottom {
            canvas.fill_rounded_rect(r, 3.0, theme.track);
        }
        y += bar + state.scaled(8);
    }
}

fn scroll_bar(canvas: &mut dyn Canvas, bounds: Rect, state: &ViewState, theme: &Theme) {
    let Some((at, len)) = state.thumb() else {
        return;
    };
    let bar = state.scaled(BAR_96);
    let track = Rect::new(bounds.right - bar, bounds.top, bounds.right, bounds.bottom);
    canvas.fill_rect_rgba(track, Rgba::from(theme.scrollbar_track));
    let thumb = Rect::new(
        track.left + 2,
        bounds.top + at + 2,
        track.right - 2,
        bounds.top + at + len - 2,
    );
    canvas.fill_rounded_rect(thumb, 3.0, theme.scrollbar);
}

fn within(r: Rect, bounds: Rect) -> Rect {
    Rect::new(
        r.left.max(bounds.left),
        r.top.max(bounds.top),
        r.right.min(bounds.right),
        r.bottom.min(bounds.bottom),
    )
}

fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<ViewState>>,
    event: &Event,
) -> Option<Msg> {
    // `panic = "abort"`: an event that arrives while the view is borrowed (a
    // paint or a tick holds it) is dropped and the view repainted: a lost
    // wheel notch is invisible, a crash is not.
    let Ok(mut s) = state.try_borrow_mut() else {
        ui.invalidate(id);
        return None;
    };
    let outcome = route(ui, id, &mut s, event);
    drop(s);
    match outcome {
        Outcome::Nothing => None,
        Outcome::Moved => {
            if let Ok(mut s) = state.try_borrow_mut() {
                s.moved = true;
            }
            ui.invalidate(id);
            None
        }
        Outcome::Action(action) => Some(Msg::Transcript(action)),
    }
}

enum Outcome {
    Nothing,
    Moved,
    Action(ViewAction),
}

fn scrolled(s: &mut ViewState, dy: i32) -> Outcome {
    let before = s.transcript.scroll_top();
    s.transcript.scroll_by(dy);
    if s.transcript.scroll_top() == before {
        Outcome::Nothing
    } else {
        Outcome::Moved
    }
}

fn route(ui: &Ui<Msg>, id: WidgetId, s: &mut ViewState, event: &Event) -> Outcome {
    match *event {
        Event::MouseWheel {
            delta, horizontal, ..
        } if !horizontal => {
            let step = -i32::from(delta) * s.scaled(WHEEL_96) / WHEEL_DELTA;
            scrolled(s, step)
        }
        Event::KeyDown { key, modifiers, .. } if !modifiers.ctrl && !modifiers.alt => {
            let page = (s.height * 9 / 10).max(1);
            let line = s.scaled(WHEEL_96);
            match key {
                Key::PAGE_DOWN | Key::SPACE => scrolled(s, page),
                Key::PAGE_UP => scrolled(s, -page),
                Key::DOWN => scrolled(s, line),
                Key::UP => scrolled(s, -line),
                Key::HOME => scrolled(s, -s.transcript.scroll_top()),
                Key::END => scrolled(s, s.transcript.max_scroll()),
                _ => Outcome::Nothing,
            }
        }
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => {
            ui.focus(id);
            let bar = s.scaled(BAR_96);
            if x >= s.width - bar {
                return bar_press(ui, id, s, y);
            }
            click(s, x, y)
        }
        Event::MouseMove { y, .. } => {
            let Some(grab) = s.grab else {
                return Outcome::Nothing;
            };
            let (total, view) = (s.transcript.total_height(), s.height.max(1));
            let ratio = total as f32 / view as f32;
            let to = grab.scroll + ((y - grab.from) as f32 * ratio) as i32;
            let before = s.transcript.scroll_top();
            s.transcript.set_scroll_top(to);
            if s.transcript.scroll_top() == before {
                Outcome::Nothing
            } else {
                Outcome::Moved
            }
        }
        Event::MouseUp {
            button: MouseButton::Left,
            ..
        } => {
            if s.grab.take().is_some() {
                ui.release_capture();
            }
            Outcome::Nothing
        }
        Event::CaptureChanged | Event::MouseLeave => {
            s.grab = None;
            Outcome::Nothing
        }
        _ => Outcome::Nothing,
    }
}

fn bar_press(ui: &Ui<Msg>, id: WidgetId, s: &mut ViewState, y: i32) -> Outcome {
    let Some((at, len)) = s.thumb() else {
        return Outcome::Nothing;
    };
    if y >= at && y < at + len {
        s.grab = Some(Grab {
            from: y,
            scroll: s.transcript.scroll_top(),
        });
        ui.set_capture(id);
        return Outcome::Nothing;
    }
    let page = (s.height * 9 / 10).max(1);
    scrolled(s, if y < at { -page } else { page })
}

/// A click at view-local `(x, y)`: a hit region of the row under it.
fn click(s: &ViewState, x: i32, y: i32) -> Outcome {
    let t = &s.transcript;
    let content_y = t.scroll_top() + y;
    let Some(i) = t.row_at(content_y) else {
        return Outcome::Nothing;
    };
    let Some(row) = t.row(i) else {
        return Outcome::Nothing;
    };
    let (lx, ly) = (x, content_y - t.row_top(i));
    let hit = row.prepared.as_ref().and_then(|p| {
        p.hits.iter().find(|h| {
            lx >= h.rect.left && lx < h.rect.right && ly >= h.rect.top && ly < h.rect.bottom
        })
    });
    match hit.map(|h| &h.target) {
        Some(Target::Link(url)) => Outcome::Action(ViewAction::Link(url.clone())),
        Some(Target::Copy(text)) => Outcome::Action(ViewAction::Copy(text.clone())),
        Some(Target::Toggle) => Outcome::Action(ViewAction::Toggle(row.id)),
        None => Outcome::Nothing,
    }
}
