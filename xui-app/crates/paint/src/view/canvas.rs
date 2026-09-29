#![forbid(unsafe_code)]

//! The canvas widget: a custom-painted node that shows the bitmap, a live shape
//! preview and a brush cursor, and maps pointer input to canvas coordinates.

mod paint;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::Rect;
use xui_core::image::Image;
use xui_core::message::MouseButton;
use xui_core::widget::Control;

use super::Msg;
use crate::model::{Bitmap, Preview, Side};

// xui gap: G1 — a `Custom` node with a painter and event closures is built from
// the public `Control` primitive rather than a dedicated canvas widget.
/// A canvas interaction, in canvas pixels (the bitmap's own coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasMsg {
    /// A button went down.
    Down {
        /// Canvas x.
        x: i32,
        /// Canvas y.
        y: i32,
        /// Which paint side the button selects.
        side: Side,
    },
    /// The pointer moved while a button may be down.
    Move {
        /// Canvas x.
        x: i32,
        /// Canvas y.
        y: i32,
    },
    /// A button went up at the given canvas position.
    Up {
        /// Canvas x.
        x: i32,
        /// Canvas y.
        y: i32,
    },
    /// The drag was interrupted (focus or capture lost).
    Cancel,
}

/// What the canvas painter reads. Kept separate from the model so a painter
/// never borrows the model or mutates it.
#[derive(Default)]
pub struct CanvasState {
    image: Option<Image>,
    revision: u64,
    offset: (i32, i32),
    bitmap_size: (i32, i32),
    preview: Option<Preview>,
    cursor: Option<(i32, i32)>,
    brush: u32,
}

/// A scrollable viewport over the model's bitmap.
pub struct PaintCanvas {
    control: Control<Msg>,
    state: Rc<RefCell<CanvasState>>,
}

impl PaintCanvas {
    /// Creates a canvas node at `bounds`.
    pub fn new(ui: &Ui<Msg>, bounds: Rect) -> Result<PaintCanvas> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, bounds).tab_stop())?;
        let state: Rc<RefCell<CanvasState>> = Rc::new(RefCell::new(CanvasState::default()));
        let releasing = Rc::new(Cell::new(false));

        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                paint::paint(canvas, &state.borrow(), &theme.get());
            }));
        }

        {
            let state = Rc::clone(&state);
            let releasing = Rc::clone(&releasing);
            let ui = ui.clone();
            let id = control.id();
            control.on_events(move |event| {
                if ui.is_design_mode() && event.is_input() {
                    return None;
                }
                handle(&ui, id, &state, &releasing, event)
            });
        }

        Ok(PaintCanvas { control, state })
    }

    /// The canvas's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Refreshes the painter's cached image and preview from `bitmap`/`revision`
    /// and the model's preview, rebuilding the [`Image`] only when the bitmap
    /// changed.
    pub fn sync(&self, bitmap: &Bitmap, revision: u64, preview: Option<Preview>, brush: u32) {
        let changed = {
            let mut state = self.state.borrow_mut();
            let changed = state.revision != revision;
            if changed {
                // xui gap: G5 — no dirty rect reaches a painter, so the whole
                // node repaints; cache the Image by revision to stay cheap.
                state.image = Some(bitmap.to_image());
                state.revision = revision;
            }
            state.bitmap_size = (bitmap.width() as i32, bitmap.height() as i32);
            if state.preview != preview || state.brush != brush {
                state.preview = preview;
                state.brush = brush;
                self.control.invalidate();
            }
            changed
        };
        if changed {
            self.control.invalidate();
        }
    }

    /// The current scroll offset in device pixels.
    pub fn offset(&self) -> (i32, i32) {
        self.state.borrow().offset
    }

    /// Moves/resizes the node.
    pub fn set_bounds(&self, bounds: Rect) {
        self.control.set_bounds(bounds);
    }

    /// Shows or hides the node.
    pub fn set_visible(&self, visible: bool) {
        self.control.set_visible(visible);
    }
}

/// Converts a local pointer position to a canvas position.
fn canvas_at(state: &CanvasState, x: i32, y: i32) -> (i32, i32) {
    (
        x.saturating_add(state.offset.0),
        y.saturating_add(state.offset.1),
    )
}

/// Maps which side a button paints.
fn side_of(button: MouseButton) -> Option<Side> {
    match button {
        MouseButton::Left => Some(Side::Primary),
        MouseButton::Right => Some(Side::Secondary),
        _ => None,
    }
}

/// The canvas event mapper.
fn handle(
    ui: &Ui<Msg>,
    id: WidgetId,
    state: &Rc<RefCell<CanvasState>>,
    releasing: &Rc<Cell<bool>>,
    event: &Event,
) -> Option<Msg> {
    match event {
        Event::MouseDown { x, y, button, .. } => {
            let side = side_of(*button)?;
            ui.focus(id);
            ui.set_capture(id);
            let (cx, cy) = canvas_at(&state.borrow(), *x, *y);
            Some(Msg::Canvas(CanvasMsg::Down { x: cx, y: cy, side }))
        }
        Event::MouseMove { x, y, .. } => {
            let (cx, cy) = {
                let mut state = state.borrow_mut();
                let at = canvas_at(&state, *x, *y);
                state.cursor = Some(at);
                at
            };
            ui.invalidate(id);
            Some(Msg::Canvas(CanvasMsg::Move { x: cx, y: cy }))
        }
        Event::MouseUp { x, y, .. } => {
            let at = canvas_at(&state.borrow(), *x, *y);
            // xui gap: G2 — release_capture raises a synchronous CaptureChanged
            // that must not read as a fresh interruption.
            releasing.set(true);
            ui.release_capture();
            releasing.set(false);
            Some(Msg::Canvas(CanvasMsg::Up { x: at.0, y: at.1 }))
        }
        Event::MouseLeave => {
            // Only hide the brush ring: with capture held, a leave does not end
            // a drag (a source that emits one anyway must not cancel it).
            let had = state.borrow_mut().cursor.take().is_some();
            if had {
                ui.invalidate(id);
            }
            None
        }
        Event::CaptureChanged | Event::KillFocus => {
            if releasing.get() {
                return None;
            }
            state.borrow_mut().cursor = None;
            ui.invalidate(id);
            Some(Msg::Canvas(CanvasMsg::Cancel))
        }
        Event::MouseWheel {
            delta, horizontal, ..
        } => {
            let step = i32::from(*delta) * 24;
            if *horizontal {
                scroll_ui(ui, id, state, step, 0);
            } else {
                scroll_ui(ui, id, state, 0, step);
            }
            None
        }
        _ => None,
    }
}

/// Scrolls the viewport, clamped to the bitmap.
fn scroll_ui(ui: &Ui<Msg>, id: WidgetId, state: &Rc<RefCell<CanvasState>>, dx: i32, dy: i32) {
    let (bitmap, bounds) = {
        let state = state.borrow();
        (state.bitmap_size, ui.bounds(id))
    };
    let mut state = state.borrow_mut();
    let next = (
        (state.offset.0 + dx).clamp(0, (bitmap.0 - bounds.width()).max(0)),
        (state.offset.1 + dy).clamp(0, (bitmap.1 - bounds.height()).max(0)),
    );
    if next != state.offset {
        state.offset = next;
        drop(state);
        ui.invalidate(id);
    }
}

#[cfg(test)]
mod tests;
