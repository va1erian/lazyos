#![forbid(unsafe_code)]

//! The canvas widget: a custom-painted node that shows the picture at the
//! viewport's zoom, a live shape preview and a brush cursor, and maps pointer
//! input to picture coordinates.

mod events;
mod paint;
mod state;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};

use super::Msg;
use crate::model::{Model, Side};
use crate::viewport::Viewport;

pub use state::CanvasState;

// xui gap: G1 — a `Custom` node with a painter and event closures is built from
// the public `Control` primitive rather than a dedicated canvas widget.
/// A canvas interaction, in picture pixels (the document's own coordinates,
/// whatever the zoom).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasMsg {
    /// A button went down.
    Down {
        /// Picture x.
        x: i32,
        /// Picture y.
        y: i32,
        /// Which paint side the button selects.
        side: Side,
    },
    /// The pointer moved while a button may be down.
    Move {
        /// Picture x.
        x: i32,
        /// Picture y.
        y: i32,
    },
    /// A button went up at the given picture position.
    Up {
        /// Picture x.
        x: i32,
        /// Picture y.
        y: i32,
    },
    /// The drag was interrupted (focus or capture lost).
    Cancel,
    /// The canvas zoomed or scrolled itself (wheel, Space-drag).
    View,
}

/// A zoomable, scrollable view over the model's document.
pub struct PaintCanvas {
    control: Control<Msg>,
    state: Rc<RefCell<CanvasState>>,
}

impl PaintCanvas {
    /// Creates a canvas node; the window layout places it.
    pub fn new(ui: &Ui<Msg>) -> Result<PaintCanvas> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state: Rc<RefCell<CanvasState>> = Rc::new(RefCell::new(CanvasState::default()));
        let releasing = Rc::new(Cell::new(false));

        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                paint::paint(canvas, &mut state.borrow_mut(), &theme.get());
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
                events::handle(&ui, id, &state, &releasing, event)
            });
        }

        Ok(PaintCanvas { control, state })
    }

    /// The canvas's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Brings the view up to date with the model: the flattened cache takes
    /// only the rectangles the model reports changed, the screen image is
    /// rebuilt lazily by the painter, and a shrinking document cannot leave the
    /// viewport scrolled past it.
    pub fn sync(&self, model: &Model) {
        let mut state = self.state.borrow_mut();
        let mut changed = state.update(model.document(), model.take_damage());
        // The model may have been replaced wholesale (New, Open): keep the
        // viewport's picture size in step.
        let preview = model.preview();
        let brush = model.size();
        if state.preview != preview || state.brush != brush {
            state.preview = preview;
            state.brush = brush;
            changed = true;
        }
        drop(state);
        if changed {
            self.control.invalidate();
        }
    }

    /// The viewport (zoom and scroll).
    pub fn viewport(&self) -> Viewport {
        *self.state.borrow().viewport()
    }

    /// The zoom as a whole percent.
    pub fn zoom_percent(&self) -> u32 {
        self.state.borrow().viewport().percent()
    }

    /// The scroll offset in view pixels.
    pub fn offset(&self) -> (i32, i32) {
        self.state.borrow().viewport().origin()
    }

    /// Zooms in one step around the centre of the view.
    pub fn zoom_in(&self) {
        self.zoom(|vp, doc, view| vp.zoom_in((view.0 / 2, view.1 / 2), doc, view));
    }

    /// Zooms out one step around the centre of the view.
    pub fn zoom_out(&self) {
        self.zoom(|vp, doc, view| vp.zoom_out((view.0 / 2, view.1 / 2), doc, view));
    }

    /// Zooms so the whole picture fits the view.
    pub fn zoom_fit(&self) {
        self.zoom(|vp, doc, view| vp.fit(doc, view));
    }

    /// Zooms to 100%.
    pub fn actual_size(&self) {
        self.zoom(|vp, doc, view| vp.actual_size(doc, view));
    }

    /// Shows or hides the pixel grid (drawn from 800%).
    pub fn set_grid(&self, grid: bool) {
        self.state.borrow_mut().set_grid(grid);
        self.control.invalidate();
    }

    fn zoom(&self, change: impl FnOnce(&mut Viewport, (u32, u32), (i32, i32))) {
        let moved = self.state.borrow_mut().change_viewport(change);
        if moved {
            self.control.invalidate();
        }
    }

    /// Shows or hides the node.
    pub fn set_visible(&self, visible: bool) {
        self.control.set_visible(visible);
    }
}

impl Placeable<Msg> for PaintCanvas {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The canvas takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    /// A smaller view must not stay scrolled past the picture.
    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        if self.state.borrow_mut().set_view(rect) {
            self.control.invalidate();
        }
    }
}

#[cfg(test)]
mod tests;
