//! FeedView (docs/lazysky-plan.md §6): one custom-painted control that draws
//! every list of posts. It is virtualized (paint is O(visible rows), R4),
//! scrolls by anchor (R3) and never shapes text itself: rows come in
//! prepared (R2).
//!
//! Modelled on the PDF Viewer's `PageView`: a painter closure over shared
//! state, `on_events`, its own scroll bar and wheel step. The app owns the
//! timer; [`FeedView::autoscroll_step`] is called from its tick.

mod events;
pub mod paint;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use lazysky_core::feed::{Feed, RowId};
use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};

use crate::images::ImageCache;
use crate::perf::Perf;
use crate::prepare::{Prepared, Target};
use crate::Msg;

/// One wheel notch, in pixels at 96 dpi.
pub const WHEEL_96: i32 = 48;
/// The scroll bar's thickness at 96 dpi.
pub const BAR_96: i32 = 10;
/// The autoscroll dead zone around the click, at 96 dpi.
pub const DEAD_96: i32 = 10;
/// Autoscroll speed: pixels per tick per pixel beyond the dead zone.
pub const GAIN: f32 = 0.1;

/// What a click in the feed means; the app decides what to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedAction {
    /// A hit region of a row (a link, a picture, a button).
    Target {
        row: RowId,
        target: Target,
        /// Where in the view it was clicked (view-local pixels).
        at: (i32, i32),
    },
    /// A right click on a hit region of a row.
    Context {
        row: RowId,
        target: Target,
        at: (i32, i32),
    },
    /// Elsewhere in a row.
    Row(RowId),
}

/// How the scroll bar's thumb is held.
#[derive(Clone, Copy)]
pub(crate) struct Grab {
    /// The pointer's y when it grabbed.
    pub from: i32,
    /// The scroll position then.
    pub scroll: i32,
}

pub struct ViewState {
    pub feed: Feed<Prepared>,
    /// Pictures ready to draw (shared by every pane).
    pub images: Rc<RefCell<ImageCache>>,
    pub dpi: u32,
    /// The tallest a lone picture is laid out, in design pixels (a setting).
    pub max_image_h: i32,
    /// The view in device pixels.
    pub width: i32,
    pub height: i32,
    /// The width changed: rows need laying out again.
    pub relayout: bool,
    /// The position changed: dehydration and refresh are due.
    pub moved: bool,
    pub(crate) grab: Option<Grab>,
    /// Middle-button autoscroll: where it started and where the pointer is.
    pub(crate) autoscroll: Option<(i32, i32)>,
    /// A note shown over an empty feed.
    pub note: String,
}

impl ViewState {
    pub fn new() -> ViewState {
        ViewState::with_images(Rc::default())
    }

    pub fn with_images(images: Rc<RefCell<ImageCache>>) -> ViewState {
        ViewState {
            feed: Feed::new(),
            images,
            dpi: 96,
            max_image_h: crate::prepare::DEFAULT_MAX_IMAGE_H,
            width: 0,
            height: 0,
            relayout: false,
            moved: false,
            grab: None,
            autoscroll: None,
            note: String::new(),
        }
    }

    pub(crate) fn scaled(&self, design: i32) -> i32 {
        (design * self.dpi.max(1) as i32 + 48) / 96
    }

    /// The width rows are laid out at: the view less the scroll bar.
    pub fn content_width(&self) -> i32 {
        (self.width - self.scaled(BAR_96)).max(1)
    }

    /// Sets the size the control was given.
    pub fn set_view(&mut self, width: i32, height: i32, dpi: u32) {
        if width != self.width || dpi != self.dpi {
            self.relayout = true;
        }
        self.width = width;
        self.height = height;
        self.dpi = dpi;
        self.feed.set_view_height(height);
        self.moved = true;
    }
}

impl Default for ViewState {
    fn default() -> ViewState {
        ViewState::new()
    }
}

pub struct FeedView {
    control: Control<Msg>,
    ui: Ui<Msg>,
    pub state: Rc<RefCell<ViewState>>,
    pub perf: Rc<RefCell<Perf>>,
}

impl FeedView {
    pub fn new(ui: &Ui<Msg>, images: Rc<RefCell<ImageCache>>, pane: u8) -> Result<FeedView> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state = Rc::new(RefCell::new(ViewState::with_images(images)));
        let perf = Rc::new(RefCell::new(Perf::default()));
        {
            let (state, perf, theme) = (Rc::clone(&state), Rc::clone(&perf), ui.theme_handle());
            control.set_painter(Rc::new(move |canvas| {
                let started = Instant::now();
                // Never borrowed across a call back into xui; if a backend
                // paints from inside an event, the next paint catches up.
                if let Ok(state) = state.try_borrow() {
                    paint::paint(canvas, &state, &theme.get());
                }
                if let Ok(mut perf) = perf.try_borrow_mut() {
                    perf.paint(started.elapsed());
                }
            }));
        }
        {
            let (state, ui, id) = (Rc::clone(&state), ui.clone(), control.id());
            control.on_events(move |event| events::handle(&ui, id, &state, pane, event));
        }
        Ok(FeedView {
            control,
            ui: ui.clone(),
            state,
            perf,
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

    /// Whether the middle-button autoscroll is running (the app keeps its
    /// timer fast while it is).
    pub fn autoscrolling(&self) -> bool {
        self.state.borrow().autoscroll.is_some()
    }

    /// One tick of autoscroll; repaints when it moved.
    pub fn autoscroll_step(&self) {
        let moved = {
            let mut s = self.state.borrow_mut();
            let Some((origin, current)) = s.autoscroll else {
                return;
            };
            let dead = s.scaled(DEAD_96);
            let off = current - origin;
            let beyond = (off.abs() - dead).max(0);
            let step = (beyond as f32 * GAIN).round() as i32 * off.signum();
            let before = s.feed.scroll_top();
            s.feed.scroll_by(step);
            let moved = s.feed.scroll_top() != before;
            s.moved |= moved;
            moved
        };
        if moved {
            self.invalidate();
        }
    }
}

impl Placeable<Msg> for FeedView {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The view takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        let dpi = self.control.dpi();
        self.state
            .borrow_mut()
            .set_view(rect.width(), rect.height(), dpi);
        self.invalidate();
    }
}
