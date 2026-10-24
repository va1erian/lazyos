//! The tab strip (docs/lazyweb-plan.md 2.1): a row of tabs above the toolbar, each with a spinner
//! while it loads, a title and a close button, and a button for a new tab at the end. It is custom
//! painted, like the throbber and the suggestions, because a tab strip has a shape no stock widget
//! has. It holds only what it shows (`Item`); the model is `tabs.rs`, and a click is a [`TabMsg`] the
//! window answers.
//!
//! Where a click lands is [`Geometry::hit`], pure and host-tested.

use std::cell::{Cell, RefCell};
use std::f32::consts::TAU;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{
    Event, NodeKind, NodeSpec, Result, TextAlign, TextStyle, TextVAlign, TimerId, WidgetId,
};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Dip, MouseButton};

use crate::app::Msg;
use crate::tabs::TabId;

/// The strip's height, in design units.
pub const HEIGHT: Dip = Dip(30.0);
/// A tab's width when there is room, and the least it is given.
const TAB_MAX: Dip = Dip(220.0);
const TAB_MIN: Dip = Dip(44.0);
/// The new tab button's width.
const PLUS: Dip = Dip(32.0);
/// The close button's side and its distance from the tab's right edge.
const CLOSE: Dip = Dip(18.0);
const CLOSE_INSET: Dip = Dip(6.0);
/// A tab narrower than this has its close button on the front tab only.
const CLOSE_ALWAYS: Dip = Dip(90.0);
/// The spinner's side, its distance from the left edge, and the text's size.
const SPIN: Dip = Dip(12.0);
const SPIN_INSET: Dip = Dip(8.0);
const TEXT: Dip = Dip(12.5);
const DOTS: usize = 8;
const STEP_MS: u32 = 90;

/// What the strip and the window's tab keys ask of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabMsg {
    /// Bring this tab to the front.
    Select(TabId),
    /// Close this tab (its button, or a middle click).
    Close(TabId),
    /// A new tab (the strip's button, Ctrl+T).
    New,
    /// Close the tab in front (Ctrl+W).
    CloseFront,
    /// Bring the last closed tab back (Ctrl+Shift+T).
    Reopen,
    /// Ctrl+Tab and Ctrl+Shift+Tab.
    Next,
    Previous,
    /// Ctrl+1..9.
    Jump(u8),
    /// A middle click or Ctrl+click on the link under the pointer: open it in a background tab.
    OpenHovered,
}

/// What a tab shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: TabId,
    pub title: String,
    pub loading: bool,
    /// The tab's page stopped: the title says so.
    pub crashed: bool,
    /// No view is held: the title is dimmed.
    pub discarded: bool,
}

/// What a point of the strip is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    Close(usize),
    New,
    Nothing,
}

/// Where the tabs are, in pixels for a strip `width` wide holding `count` tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub count: usize,
    pub tab: i32,
    pub plus: i32,
    pub close: i32,
    pub close_inset: i32,
    /// Whether every tab has a close button, or only the front one.
    pub close_all: bool,
    pub width: i32,
}

impl Geometry {
    pub fn new(count: usize, width: i32, dpi: u32) -> Geometry {
        let px = |dip: Dip| dip.to_px(dpi).value();
        let plus = px(PLUS);
        let room = (width - plus).max(0);
        let share = if count == 0 {
            px(TAB_MAX)
        } else {
            room / count as i32
        };
        let tab = share.clamp(px(TAB_MIN), px(TAB_MAX));
        Geometry {
            count,
            tab,
            plus,
            close: px(CLOSE),
            close_inset: px(CLOSE_INSET),
            close_all: tab >= px(CLOSE_ALWAYS),
            width,
        }
    }

    /// The tab `n`'s left edge.
    pub fn left(&self, n: usize) -> i32 {
        n as i32 * self.tab
    }

    /// The tab `n`'s close button, inside it, centred on `height`.
    pub fn close_rect(&self, n: usize, height: i32) -> Rect {
        let right = self.left(n) + self.tab - self.close_inset;
        let top = (height - self.close) / 2;
        Rect::new(right - self.close, top, right, top + self.close)
    }

    /// The new tab button.
    pub fn plus_rect(&self, height: i32) -> Rect {
        let left = self.left(self.count).min((self.width - self.plus).max(0));
        Rect::new(left, 0, left + self.plus, height)
    }

    /// What the point `x` (and `y`, in a strip `height` tall) is on. A tab past the right edge
    /// (they do not all fit) cannot be hit.
    pub fn hit(&self, x: i32, y: i32, height: i32) -> Hit {
        if x < 0 || y < 0 || y >= height {
            return Hit::Nothing;
        }
        let plus = self.plus_rect(height);
        if x >= plus.left && x < plus.right {
            return Hit::New;
        }
        let n = (x / self.tab.max(1)) as usize;
        if n >= self.count || x >= self.width {
            return Hit::Nothing;
        }
        let close = self.close_rect(n, height);
        if x >= close.left && x < close.right && y >= close.top && y < close.bottom {
            Hit::Close(n)
        } else {
            Hit::Tab(n)
        }
    }
}

#[derive(Default)]
struct View {
    items: Vec<Item>,
    front: Option<TabId>,
    hover: Option<Hit>,
    phase: usize,
}

pub struct TabStrip {
    control: Control<Msg>,
    view: Rc<RefCell<View>>,
    timer: Cell<Option<TimerId>>,
}

/// A title that fits `room` px at about `per` px a letter, with an ellipsis when cut.
fn fitted(title: &str, room: i32, per: i32) -> String {
    let fits = usize::try_from(room / per.max(1)).unwrap_or(0).max(1);
    if title.chars().count() <= fits {
        return title.to_owned();
    }
    let mut cut: String = title.chars().take(fits.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

impl TabStrip {
    pub fn new(ui: &Ui<Msg>) -> Result<TabStrip> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        let view = Rc::new(RefCell::new(View::default()));
        {
            let view = Rc::clone(&view);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                let theme = theme.get();
                let bounds = canvas.bounds();
                canvas.fill_rect(bounds, theme.background);
                let view = view.borrow();
                let dpi = canvas.dpi();
                let height = bounds.height();
                let g = Geometry::new(view.items.len(), bounds.width(), dpi);
                for (n, item) in view.items.iter().enumerate() {
                    let left = bounds.left + g.left(n);
                    if left >= bounds.right {
                        break;
                    }
                    let cell = Rect::new(
                        left,
                        bounds.top,
                        (left + g.tab).min(bounds.right),
                        bounds.bottom,
                    );
                    let front = view.front == Some(item.id);
                    if front {
                        canvas.fill_rect(cell, theme.raised);
                        let rule = Rect::new(cell.left, cell.bottom - 2, cell.right, cell.bottom);
                        canvas.fill_rect(rule, theme.accent);
                    } else if matches!(view.hover, Some(Hit::Tab(h) | Hit::Close(h)) if h == n) {
                        canvas.fill_rect(cell, theme.hover);
                    }
                    canvas.draw_line(
                        Point::new(cell.right - 1, cell.top + height / 5),
                        Point::new(cell.right - 1, cell.bottom - height / 5),
                        theme.border,
                        1.0,
                    );
                    let inset = SPIN_INSET.to_px(dpi).value();
                    let spin = SPIN.to_px(dpi).value();
                    if item.loading {
                        spinner(canvas, cell, inset, spin, view.phase, &theme);
                    }
                    let shows_close = g.close_all || front;
                    let text_left =
                        cell.left + inset + if item.loading { spin + inset / 2 } else { 0 };
                    let text_right = cell.right
                        - if shows_close {
                            g.close + g.close_inset
                        } else {
                            inset
                        };
                    if text_right > text_left {
                        let mut style = TextStyle::new(
                            if item.discarded {
                                theme.text_disabled
                            } else {
                                theme.text
                            },
                            TEXT,
                        );
                        style.valign = TextVAlign::Middle;
                        style.align = TextAlign::Start;
                        let per = (TEXT.to_px(dpi).value() * 11 / 20).max(1);
                        let title = if item.crashed {
                            format!("! {}", item.title)
                        } else {
                            item.title.clone()
                        };
                        let area = Rect::new(text_left, cell.top, text_right, cell.bottom);
                        canvas.push_clip(area);
                        canvas.draw_text(&fitted(&title, area.width(), per), area, &style);
                        canvas.pop_clip();
                    }
                    if shows_close {
                        let close = g.close_rect(n, height);
                        let at = Rect::new(
                            bounds.left + close.left,
                            bounds.top + close.top,
                            bounds.left + close.right,
                            bounds.top + close.bottom,
                        );
                        if view.hover == Some(Hit::Close(n)) {
                            canvas.fill_rect(at, theme.selection);
                        }
                        cross(canvas, at, theme.text);
                    }
                }
                let plus = g.plus_rect(height);
                let at = Rect::new(
                    bounds.left + plus.left,
                    bounds.top + plus.top,
                    bounds.left + plus.right,
                    bounds.top + plus.bottom,
                );
                if view.hover == Some(Hit::New) {
                    canvas.fill_rect(at, theme.hover);
                }
                let (cx, cy) = ((at.left + at.right) / 2, (at.top + at.bottom) / 2);
                let arm = PLUS.to_px(dpi).value() / 6;
                canvas.draw_line(
                    Point::new(cx - arm, cy),
                    Point::new(cx + arm, cy),
                    theme.text,
                    1.5,
                );
                canvas.draw_line(
                    Point::new(cx, cy - arm),
                    Point::new(cx, cy + arm),
                    theme.text,
                    1.5,
                );
                canvas.draw_line(
                    Point::new(bounds.left, bounds.bottom - 1),
                    Point::new(bounds.right, bounds.bottom - 1),
                    theme.border,
                    1.0,
                );
            }));
        }
        {
            let (view, ui) = (Rc::clone(&view), ui.clone());
            let id = control.id();
            control.on_events(move |event| {
                let bounds = ui.bounds(id);
                let (width, height) = (bounds.width(), bounds.height());
                let at = |x: i32, y: i32| {
                    let count = view.borrow().items.len();
                    Geometry::new(count, width, ui.dpi()).hit(x, y, height)
                };
                match event {
                    Event::MouseMove { x, y, .. } => {
                        let hit = at(*x, *y);
                        let hover = Some(hit).filter(|h| *h != Hit::Nothing);
                        if view.borrow().hover != hover {
                            view.borrow_mut().hover = hover;
                            ui.invalidate(id);
                        }
                        None
                    }
                    Event::MouseLeave => {
                        view.borrow_mut().hover = None;
                        ui.invalidate(id);
                        None
                    }
                    Event::MouseDown { x, y, button, .. } => {
                        let view = view.borrow();
                        let item = |n: usize| view.items.get(n).map(|item| item.id);
                        let msg = match (at(*x, *y), button) {
                            (Hit::Tab(n), MouseButton::Left) => item(n).map(TabMsg::Select),
                            (Hit::Close(n), MouseButton::Left) => item(n).map(TabMsg::Close),
                            (Hit::Tab(n) | Hit::Close(n), MouseButton::Middle) => {
                                item(n).map(TabMsg::Close)
                            }
                            (Hit::New, MouseButton::Left) => Some(TabMsg::New),
                            _ => None,
                        };
                        msg.map(Msg::Tab)
                    }
                    _ => None,
                }
            });
        }
        Ok(TabStrip {
            control,
            view,
            timer: Cell::new(None),
        })
    }

    /// Shows `items`, `front` highlighted. The spinners turn only while one tab loads.
    pub fn set(&self, items: Vec<Item>, front: Option<TabId>) {
        let loading = items.iter().any(|item| item.loading);
        {
            let mut view = self.view.borrow_mut();
            if view.items == items && view.front == front {
                return;
            }
            view.items = items;
            view.front = front;
        }
        self.spin(loading);
        self.control.invalidate();
    }

    /// What the strip shows, for a test.
    pub fn items(&self) -> Vec<Item> {
        self.view.borrow().items.clone()
    }

    /// Where tab `n` and its close button are in the window, for a session script and the tests.
    pub fn rects(&self, ui: &Ui<Msg>, n: usize) -> (Rect, Rect) {
        let bounds = ui.bounds(self.control.id());
        let count = self.view.borrow().items.len();
        let g = Geometry::new(count, bounds.width(), ui.dpi());
        let left = bounds.left + g.left(n);
        let tab = Rect::new(left, bounds.top, left + g.tab, bounds.bottom);
        let close = g.close_rect(n, bounds.height());
        let close = Rect::new(
            bounds.left + close.left,
            bounds.top + close.top,
            bounds.left + close.right,
            bounds.top + close.bottom,
        );
        (tab, close)
    }

    /// The new tab button in the window.
    pub fn plus(&self, ui: &Ui<Msg>) -> Rect {
        let bounds = ui.bounds(self.control.id());
        let count = self.view.borrow().items.len();
        let plus = Geometry::new(count, bounds.width(), ui.dpi()).plus_rect(bounds.height());
        Rect::new(
            bounds.left + plus.left,
            bounds.top,
            bounds.left + plus.right,
            bounds.bottom,
        )
    }

    fn spin(&self, on: bool) {
        if on && self.timer.get().is_none() {
            let (view, ui) = (Rc::clone(&self.view), self.control.ui().clone());
            let id = self.control.id();
            self.timer.set(self.control.set_timer(STEP_MS, move || {
                let mut view = view.borrow_mut();
                view.phase = (view.phase + 1) % DOTS;
                ui.invalidate(id);
                None
            }));
        } else if !on {
            if let Some(timer) = self.timer.take() {
                self.control.kill_timer(timer);
            }
        }
    }
}

/// A ring of dots, the lead one the accent, in the `side` square `inset` from `cell`'s left.
fn spinner(
    canvas: &mut dyn xui_core::backend::Canvas,
    cell: Rect,
    inset: i32,
    side: i32,
    phase: usize,
    theme: &xui_core::Theme,
) {
    let centre = Point::new(cell.left + inset + side / 2, (cell.top + cell.bottom) / 2);
    let (ring, dot) = (side as f32 * 0.4, (side as f32 * 0.12).max(1.0));
    for i in 0..DOTS {
        let angle = i as f32 / DOTS as f32 * TAU;
        let at = Point::new(
            centre.x + (angle.cos() * ring).round() as i32,
            centre.y + (angle.sin() * ring).round() as i32,
        );
        let behind = (phase + DOTS - i) % DOTS;
        let colour = theme
            .accent
            .lerp(theme.background, behind as f32 / DOTS as f32);
        canvas.fill_ellipse(at, dot, dot, colour);
    }
}

/// An X in `rect`.
fn cross(canvas: &mut dyn xui_core::backend::Canvas, rect: Rect, colour: xui_core::Color) {
    let arm = rect.width() / 4;
    let (cx, cy) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
    canvas.draw_line(
        Point::new(cx - arm, cy - arm),
        Point::new(cx + arm, cy + arm),
        colour,
        1.2,
    );
    canvas.draw_line(
        Point::new(cx - arm, cy + arm),
        Point::new(cx + arm, cy - arm),
        colour,
        1.2,
    );
}

impl Placeable<Msg> for TabStrip {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, ui: &Ui<Msg>, constraints: Constraints) -> Size {
        Size::new(
            constraints.max_width.unwrap_or(0),
            HEIGHT.to_px(ui.dpi()).value(),
        )
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        self.control.set_bounds(rect);
    }
}

#[cfg(test)]
mod tests;
