#![forbid(unsafe_code)]

//! The tool grid: a custom-painted block of tool cells, two to a row, down the
//! left of the window.
//!
//! xui's [`Toolbar`](xui_core::widget::Toolbar) has no persistent selected state
//! and no way to report which mouse button or modifier was used, so the grid is
//! one [`NodeKind::Custom`](xui_core::backend::NodeKind::Custom) node here that
//! paints its own glyphs and maps clicks to [`Msg`].
//!
//! xui's [`Tooltip`](xui_core::widget::Tooltip) attaches to a whole node, so it
//! cannot name one cell of this grid. The grid reports the hovered tool instead
//! ([`Msg::HintTool`]) and the status bar shows its name and key.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::MouseButton;
use xui_core::widget::{Control, Placeable};

use super::Msg;
use crate::model::{Model, Tool};

/// The design size of one cell.
const CELL: xui_core::Dip = xui_core::Dip(32.0);
/// How many cells a row holds.
pub const COLUMNS: i32 = 2;
/// The design width the grid asks of the layout: its cells plus a margin.
pub const WIDTH: xui_core::Dip = xui_core::Dip(COLUMNS as f32 * 32.0 + 8.0);

/// The cell size in device pixels.
fn cell_px(dpi: u32) -> i32 {
    CELL.to_px(dpi).value().max(10)
}

/// The flow rect of cell `index`: [`COLUMNS`] to a row, packed from the
/// top-left with a small margin. Pure arithmetic, so a paint or hit-test
/// allocates nothing.
fn item_rect(bounds: Rect, dpi: u32, index: usize) -> Rect {
    let cell = cell_px(dpi);
    let margin = cell / 8;
    let row = (index as i32) / COLUMNS;
    let column = (index as i32) % COLUMNS;
    let left = bounds.left + margin + column * cell;
    let top = bounds.top + margin + row * cell;
    Rect::new(left, top, left + cell, top + cell)
}

/// The grid's shared state.
struct State {
    items: Vec<Tool>,
    bounds: Cell<Rect>,
    dpi: Cell<u32>,
    active: Cell<Tool>,
    hover: Cell<Option<usize>>,
    pressed: Cell<Option<usize>>,
}

impl State {
    fn rect(&self, index: usize) -> Rect {
        item_rect(self.bounds.get(), self.dpi.get(), index)
    }

    fn index_at(&self, point: Point) -> Option<usize> {
        (0..self.items.len()).find(|index| self.rect(*index).contains(point))
    }
}

mod paint;

// xui gap: G7 — `Toolbar` has no persistent active state, G15 — no font is
// bundled, and G16 — a `Tooltip` cannot name one cell, so every cell is an icon
// drawn by this custom node and the hovered tool is named in the status bar.
/// A grid of tool cells.
pub struct ToolStrip {
    control: Control<Msg>,
    state: Rc<State>,
}

impl ToolStrip {
    /// Creates the grid of every tool; the window layout places it.
    pub fn new(ui: &Ui<Msg>) -> Result<ToolStrip> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state = Rc::new(State {
            items: Tool::ALL.to_vec(),
            bounds: Cell::new(Rect::default()),
            dpi: Cell::new(ui.dpi()),
            active: Cell::new(Tool::Pencil),
            hover: Cell::new(None),
            pressed: Cell::new(None),
        });
        let theme = ui.theme_handle();

        {
            let state = Rc::clone(&state);
            let theme = Rc::clone(&theme);
            control.set_painter(Rc::new(move |canvas| {
                paint::paint(canvas, &state, &theme.get());
            }));
        }
        {
            let state = Rc::clone(&state);
            let ui = ui.clone();
            let id = control.id();
            control.on_events(move |event| handle(&ui, id, &state, event));
        }

        Ok(ToolStrip { control, state })
    }

    /// The grid's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The device-pixel rect of `tool`'s cell, for tests.
    pub fn tool_rect(&self, tool: Tool) -> Option<Rect> {
        let index = self.state.items.iter().position(|item| *item == tool)?;
        Some(self.state.rect(index))
    }

    /// Refreshes the active tool from `model`.
    pub fn sync(&self, model: &Model) {
        self.state.active.set(model.tool());
        self.control.invalidate();
    }

    /// Shows or hides the grid.
    pub fn set_visible(&self, visible: bool) {
        self.control.set_visible(visible);
    }
}

impl Placeable<Msg> for ToolStrip {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// As tall as the cells need; the layout gives the width.
    fn measure(&self, _ui: &Ui<Msg>, constraints: Constraints) -> Size {
        let rows = (self.state.items.len() as i32 + COLUMNS - 1) / COLUMNS;
        let cell = cell_px(constraints.dpi);
        Size::new(0, rows * cell + cell / 4)
    }

    /// Hit-testing and the cell flow follow the placed rect.
    fn placed(&self, ui: &Ui<Msg>, rect: Rect) {
        self.state.bounds.set(rect);
        self.state.dpi.set(ui.dpi());
    }
}

fn handle(ui: &Ui<Msg>, id: WidgetId, state: &Rc<State>, event: &Event) -> Option<Msg> {
    match event {
        Event::MouseMove { x, y, .. } => {
            let bounds = state.bounds.get();
            let index = state.index_at(Point::new(bounds.left + x, bounds.top + y));
            if state.hover.get() == index {
                return None;
            }
            state.hover.set(index);
            ui.invalidate(id);
            Some(Msg::HintTool(index.map(|index| state.items[index])))
        }
        Event::MouseLeave | Event::CaptureChanged => {
            state.pressed.set(None);
            ui.invalidate(id);
            state.hover.replace(None).map(|_| Msg::HintTool(None))
        }
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => {
            let bounds = state.bounds.get();
            let index = state.index_at(Point::new(bounds.left + x, bounds.top + y))?;
            state.pressed.set(Some(index));
            ui.invalidate(id);
            Some(Msg::Tool(state.items[index]))
        }
        Event::MouseUp { .. } => {
            state.pressed.set(None);
            ui.invalidate(id);
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
