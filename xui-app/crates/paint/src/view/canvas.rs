#![forbid(unsafe_code)]

//! The canvas widget: a custom-painted node that shows the picture at the
//! viewport's zoom, a live shape preview and a brush cursor, and maps pointer
//! input to picture coordinates.

mod ants;
mod events;
mod paint;
mod state;
mod tweak;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};

use super::Msg;
use crate::model::{Model, Mods, Side, TextKey};
use crate::viewport::Viewport;

pub use state::CanvasState;
pub use tweak::AdjustPreview;

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
        /// The modifier keys held.
        mods: Mods,
    },
    /// The pointer moved while a button may be down.
    Move {
        /// Picture x.
        x: i32,
        /// Picture y.
        y: i32,
        /// The modifier keys held.
        mods: Mods,
    },
    /// A button went up at the given picture position.
    Up {
        /// Picture x.
        x: i32,
        /// Picture y.
        y: i32,
        /// The modifier keys held.
        mods: Mods,
    },
    /// The drag was interrupted (focus or capture lost).
    Cancel,
    /// The canvas zoomed or scrolled itself (wheel, Space-drag).
    View,
    /// Esc during a drag: take the stroke back instead of keeping it.
    Abort,
    /// A character typed while a text block is being edited.
    Char(char),
    /// An editing key pressed while a text block is being edited.
    Text(TextKey),
}

/// A zoomable, scrollable view over the model's document.
pub struct PaintCanvas {
    control: Control<Msg>,
    state: Rc<RefCell<CanvasState>>,
    ui: Ui<Msg>,
    timer: RefCell<Option<xui_core::backend::TimerId>>,
}

/// Milliseconds between steps of the marching ants.
const ANTS_MS: u32 = 200;

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

        Ok(PaintCanvas {
            control,
            state,
            ui: ui.clone(),
            timer: RefCell::new(None),
        })
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
        let brush = model.size();
        let tool = model.tool();
        let caret = model.text_caret();
        let block = model.text_block().zip(model.text_handle());
        let editing = model.is_editing_text();
        if state.brush != brush
            || state.tool != tool
            || state.caret != caret
            || state.block != block
            || state.editing != editing
        {
            state.brush = brush;
            state.tool = tool;
            state.caret = caret;
            state.block = block;
            state.editing = editing;
            changed = true;
        }
        if state.sel_rev != model.selection_rev() {
            state.sel_rev = model.selection_rev();
            state.edges = model.selection().map(|s| s.edges()).unwrap_or_default();
            changed = true;
        }
        let animate = !ants::is_empty(&state.edges) || state.editing;
        drop(state);
        self.animate_ants(animate);
        if changed {
            self.control.invalidate();
        }
    }

    /// Starts the ants' timer while there is an outline and stops it when there
    /// is not, so an idle Paint costs no wakeups. A backend with no timers
    /// leaves the ants still.
    fn animate_ants(&self, on: bool) {
        let mut timer = self.timer.borrow_mut();
        match (on, *timer) {
            (true, None) => {
                let state = Rc::clone(&self.state);
                let ui = self.ui.clone();
                let id = self.control.id();
                *timer = self.control.set_timer(ANTS_MS, move || {
                    let mut state = state.borrow_mut();
                    state.phase = state.phase.wrapping_add(1);
                    drop(state);
                    ui.invalidate(id);
                    None
                });
            }
            (false, Some(id)) => {
                self.control.kill_timer(id);
                *timer = None;
            }
            _ => {}
        }
    }

    /// The picture pixel at the top-left of the view (where a paste lands).
    pub fn view_top_left(&self) -> (i32, i32) {
        self.state.borrow().top_left()
    }

    /// Advances the ants one step, as a timer tick does.
    pub fn tick_ants(&self) {
        let mut state = self.state.borrow_mut();
        state.phase = state.phase.wrapping_add(1);
        drop(state);
        self.control.invalidate();
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

    /// Zooms in one step around the picture pixel `at` (the Zoom tool's click).
    pub fn zoom_in_at(&self, at: (i32, i32)) {
        self.zoom(|vp, doc, view| vp.zoom_in(vp.to_view(at.0, at.1), doc, view));
    }

    /// Zooms out one step around the picture pixel `at`.
    pub fn zoom_out_at(&self, at: (i32, i32)) {
        self.zoom(|vp, doc, view| vp.zoom_out(vp.to_view(at.0, at.1), doc, view));
    }

    /// Zooms so the rectangle between two picture pixels fills the view.
    pub fn zoom_to_rect(&self, a: (i32, i32), b: (i32, i32)) {
        let moved = self.state.borrow_mut().zoom_to_rect(a, b);
        if moved {
            self.control.invalidate();
        }
    }

    /// Shows or hides the Zoom tool's rubber band.
    pub fn set_marquee(&self, marquee: Option<((i32, i32), (i32, i32))>) {
        let mut state = self.state.borrow_mut();
        if state.marquee != marquee {
            state.marquee = marquee;
            drop(state);
            self.control.invalidate();
        }
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

    /// Shows `preview` on the screen image instead of the plain picture, or
    /// the plain picture again for `None`. The document is not touched.
    pub fn set_adjust_preview(&self, preview: Option<AdjustPreview>) {
        self.state.borrow_mut().set_tweak(preview);
        self.control.invalidate();
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
