#![forbid(unsafe_code)]

//! The tool strip: a custom-painted row of tool, brush-size and action cells.
//!
//! xui's [`Toolbar`](xui_core::widget::Toolbar) has no persistent selected state
//! and no way to report which mouse button or modifier was used, so the strip is
//! one [`NodeKind::Custom`](xui_core::backend::NodeKind::Custom) node here that
//! paints its own glyphs and maps clicks to [`Msg`].

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect};
use xui_core::message::MouseButton;
use xui_core::widget::Control;

use super::Msg;
use crate::model::{Model, Tool};

/// The design size of one strip cell.
const CELL: xui_core::Dip = xui_core::Dip(28.0);

/// One clickable cell in the strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripItem {
    /// Selects a tool.
    Tool(Tool),
    /// Selects a brush diameter.
    Size(u32),
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Clear the canvas.
    Clear,
    /// Start a new document.
    New,
    /// Save through the storage.
    Save,
    /// Load through the storage.
    Open,
}

impl StripItem {
    /// The ordering group, used to draw a separator between groups.
    fn group(self) -> u8 {
        match self {
            StripItem::Tool(_) => 0,
            StripItem::Size(_) => 1,
            _ => 2,
        }
    }
}

/// The cell size in device pixels.
fn cell_px(dpi: u32) -> i32 {
    CELL.to_px(dpi).value().max(10)
}

/// The flow rect of cell `index`, top-left packed and wrapping. Pure
/// arithmetic, so a paint or hit-test allocates nothing.
fn item_rect(bounds: Rect, dpi: u32, index: usize) -> Rect {
    let cell = cell_px(dpi);
    let per_row = (bounds.width() / cell).max(1) as usize;
    let row = (index / per_row) as i32;
    let column = (index % per_row) as i32;
    let left = bounds.left + column * cell;
    let top = bounds.top + row * cell;
    Rect::new(left, top, left + cell, top + cell)
}

/// The strip height needed for `count` cells at `width`, in device pixels.
pub fn preferred_height(count: usize, width: i32, dpi: u32) -> i32 {
    let cell = cell_px(dpi);
    let per_row = (width / cell).max(1) as usize;
    let rows = count.div_ceil(per_row).max(1);
    rows as i32 * cell
}

/// The strip's shared state.
struct State {
    items: Vec<StripItem>,
    bounds: Cell<Rect>,
    dpi: Cell<u32>,
    active_tool: Cell<Tool>,
    active_size: Cell<u32>,
    can_undo: Cell<bool>,
    can_redo: Cell<bool>,
    can_io: Cell<bool>,
    hover: Cell<Option<usize>>,
    pressed: Cell<Option<usize>>,
}

impl State {
    fn enabled(&self, item: StripItem) -> bool {
        match item {
            StripItem::Undo => self.can_undo.get(),
            StripItem::Redo => self.can_redo.get(),
            StripItem::Save | StripItem::Open => self.can_io.get(),
            _ => true,
        }
    }

    fn rect(&self, index: usize) -> Rect {
        item_rect(self.bounds.get(), self.dpi.get(), index)
    }

    fn index_at(&self, point: Point) -> Option<usize> {
        (0..self.items.len())
            .find(|index| self.rect(*index).contains(point))
            .filter(|index| self.enabled(self.items[*index]))
    }

    fn is_active(&self, item: StripItem) -> bool {
        match item {
            StripItem::Tool(tool) => self.active_tool.get() == tool,
            StripItem::Size(size) => self.active_size.get() == size,
            _ => false,
        }
    }
}

mod paint;

// xui gap: G7 — `Toolbar` has no persistent active state, and G15 — no font is
// bundled, so every cell is an icon or swatch drawn by this custom node.
/// A flow layout of tool, size and action cells.
pub struct ToolStrip {
    control: Control<Msg>,
    state: Rc<State>,
}

impl ToolStrip {
    /// Creates a strip of `items` at `bounds`.
    pub fn new(ui: &Ui<Msg>, bounds: Rect, items: Vec<StripItem>) -> Result<ToolStrip> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, bounds).tab_stop())?;
        let state = Rc::new(State {
            items,
            bounds: Cell::new(bounds),
            dpi: Cell::new(ui.dpi()),
            active_tool: Cell::new(Tool::Pencil),
            active_size: Cell::new(1),
            can_undo: Cell::new(false),
            can_redo: Cell::new(false),
            can_io: Cell::new(false),
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

    /// The strip's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The device-pixel rect of cell `index`, for tests.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        (index < self.state.items.len()).then(|| self.state.rect(index))
    }

    /// Refreshes the active tool/size and the enabled actions from `model`.
    pub fn sync(&self, model: &Model, storage_available: bool) {
        self.state.active_tool.set(model.tool());
        self.state.active_size.set(model.size());
        self.state.can_undo.set(model.history().can_undo());
        self.state.can_redo.set(model.history().can_redo());
        self.state.can_io.set(storage_available);
        self.control.invalidate();
    }

    /// Moves/resizes the strip.
    pub fn set_bounds(&self, bounds: Rect) {
        self.state.bounds.set(bounds);
        self.control.set_bounds(bounds);
    }

    /// Shows or hides the strip.
    pub fn set_visible(&self, visible: bool) {
        self.control.set_visible(visible);
    }
}
fn handle(ui: &Ui<Msg>, id: WidgetId, state: &Rc<State>, event: &Event) -> Option<Msg> {
    match event {
        Event::MouseMove { x, y, .. } => {
            let bounds = state.bounds.get();
            let index = state.index_at(Point::new(bounds.left + x, bounds.top + y));
            if state.hover.get() != index {
                state.hover.set(index);
                ui.invalidate(id);
            }
            None
        }
        Event::MouseLeave | Event::CaptureChanged => {
            state.hover.set(None);
            state.pressed.set(None);
            ui.invalidate(id);
            None
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
            Some(message(state.items[index]))
        }
        Event::MouseUp { .. } => {
            state.pressed.set(None);
            ui.invalidate(id);
            None
        }
        _ => None,
    }
}

/// The message an item raises.
fn message(item: StripItem) -> Msg {
    match item {
        StripItem::Tool(tool) => Msg::Tool(tool),
        StripItem::Size(size) => Msg::Size(size),
        StripItem::Undo => Msg::Undo,
        StripItem::Redo => Msg::Redo,
        StripItem::Clear => Msg::Clear,
        StripItem::New => Msg::New,
        StripItem::Save => Msg::Save,
        StripItem::Open => Msg::Open,
    }
}

#[cfg(test)]
mod tests;
