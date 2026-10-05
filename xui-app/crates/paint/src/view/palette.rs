#![forbid(unsafe_code)]

//! The colour palette: 16 fixed swatches plus the primary/secondary pair.
//!
//! `ColorPicker` reports only a chosen [`Color`](xui_core::Color) and ignores
//! which button was used, so this custom node maps left/right clicks to the
//! primary/secondary colour and paints the active pair itself.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Canvas, Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::MouseButton;
use xui_core::theme::look;
use xui_core::widget::{Control, Placeable};

use super::{Msg, color_of};
use crate::model::{Pixel, Side};

/// The design size of one palette cell.
const CELL: xui_core::Dip = xui_core::Dip(24.0);

/// The 16 fixed paint colours.
pub const PALETTE: [Pixel; 16] = [
    [0, 0, 0, 255],
    [128, 128, 128, 255],
    [128, 0, 0, 255],
    [255, 0, 0, 255],
    [255, 128, 0, 255],
    [255, 255, 0, 255],
    [0, 128, 0, 255],
    [0, 255, 0, 255],
    [0, 128, 128, 255],
    [0, 255, 255, 255],
    [0, 0, 255, 255],
    [0, 0, 128, 255],
    [128, 0, 128, 255],
    [255, 0, 255, 255],
    [128, 64, 0, 255],
    [255, 255, 255, 255],
];

fn cell_px(dpi: u32) -> i32 {
    CELL.to_px(dpi).value().max(8)
}

/// The number of rows `count` colours need at `width` device pixels.
pub fn color_rows(count: usize, width: i32, dpi: u32) -> i32 {
    let cell = cell_px(dpi);
    let available = (width - cell).max(cell);
    let columns = (available / cell).max(1) as usize;
    (count.div_ceil(columns).max(1)) as i32
}

/// The palette height needed for `count` colours at `width`.
pub fn preferred_height(count: usize, width: i32, dpi: u32) -> i32 {
    color_rows(count, width, dpi) * cell_px(dpi)
}

// xui gap: G9 — `ColorPicker` reports only a colour and ignores the button, so
// left/right cannot choose the primary/secondary swatch.
/// A palette node.
pub struct Palette {
    control: Control<Msg>,
    bounds: Rc<Cell<Rect>>,
    dpi: Rc<Cell<u32>>,
    primary: Rc<Cell<Pixel>>,
    secondary: Rc<Cell<Pixel>>,
}

impl Palette {
    /// Creates a palette; the window layout places it.
    pub fn new(ui: &Ui<Msg>) -> Result<Palette> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let bounds_cell = Rc::new(Cell::new(Rect::default()));
        let dpi = Rc::new(Cell::new(ui.dpi()));
        let primary = Rc::new(Cell::new(PALETTE[0]));
        let secondary = Rc::new(Cell::new(PALETTE[15]));
        let hover = Rc::new(Cell::new(None));
        let swap_hover = Rc::new(Cell::new(None));

        {
            let bounds = Rc::clone(&bounds_cell);
            let dpi = Rc::clone(&dpi);
            let primary = Rc::clone(&primary);
            let secondary = Rc::clone(&secondary);
            let hover = Rc::clone(&hover);
            let swap_hover = Rc::clone(&swap_hover);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                paint(
                    canvas,
                    bounds.get(),
                    dpi.get(),
                    primary.get(),
                    secondary.get(),
                    hover.get(),
                    swap_hover.get(),
                    &theme.get(),
                );
            }));
        }
        {
            let ui = ui.clone();
            let id = control.id();
            let bounds = Rc::clone(&bounds_cell);
            let dpi = Rc::clone(&dpi);
            let hover = Rc::clone(&hover);
            let swap_hover = Rc::clone(&swap_hover);
            control.on_events(move |event| match event {
                Event::MouseMove { x, y, .. } => {
                    let point = absolute(bounds.get(), *x, *y);
                    let (next_color, next_swap) = hit(bounds.get(), dpi.get(), point);
                    if hover.get() != next_color || swap_hover.get() != next_swap {
                        hover.set(next_color);
                        swap_hover.set(next_swap);
                        ui.invalidate(id);
                    }
                    None
                }
                Event::MouseLeave | Event::CaptureChanged => {
                    hover.set(None);
                    swap_hover.set(None);
                    ui.invalidate(id);
                    None
                }
                Event::MouseDown { x, y, button, .. } => {
                    let side = match button {
                        MouseButton::Left => Side::Primary,
                        MouseButton::Right => Side::Secondary,
                        _ => return None,
                    };
                    let point = absolute(bounds.get(), *x, *y);
                    let (color, swap) = hit(bounds.get(), dpi.get(), point);
                    if let Some(index) = color {
                        return Some(Msg::Palette {
                            color: PALETTE[index],
                            side,
                        });
                    }
                    swap.map(|_| Msg::SwapColors)
                }
                _ => None,
            });
        }

        Ok(Palette {
            control,
            bounds: bounds_cell,
            dpi,
            primary,
            secondary,
        })
    }

    /// The palette's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Refreshes the shown primary/secondary colours.
    pub fn sync(&self, primary: Pixel, secondary: Pixel) {
        self.primary.set(primary);
        self.secondary.set(secondary);
        self.control.invalidate();
    }

    /// The device-pixel rect of colour `index`, for tests.
    pub fn color_rect(&self, index: usize) -> Option<Rect> {
        color_rect(self.bounds.get(), self.dpi.get(), index)
    }

    /// Shows or hides the palette.
    pub fn set_visible(&self, visible: bool) {
        self.control.set_visible(visible);
    }
}

impl Placeable<Msg> for Palette {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// As tall as the colour rows need once they wrap at the offered width.
    fn measure(&self, _ui: &Ui<Msg>, constraints: Constraints) -> Size {
        let width = constraints.max_width.unwrap_or(i32::MAX);
        Size::new(0, preferred_height(PALETTE.len(), width, constraints.dpi))
    }

    /// Hit-testing and the swatch grid follow the placed rect.
    fn placed(&self, ui: &Ui<Msg>, rect: Rect) {
        self.bounds.set(rect);
        self.dpi.set(ui.dpi());
    }
}

/// Local to absolute coordinates.
fn absolute(bounds: Rect, x: i32, y: i32) -> Point {
    Point::new(bounds.left + x, bounds.top + y)
}

/// The colour cell rect for `index`.
fn color_rect(bounds: Rect, dpi: u32, index: usize) -> Option<Rect> {
    let cell = cell_px(dpi);
    let available = bounds.width() - cell;
    let columns = (available / cell).max(1) as usize;
    if index >= PALETTE.len() {
        return None;
    }
    let row = (index / columns) as i32;
    let column = (index % columns) as i32;
    let left = bounds.left + cell + column * cell;
    let top = bounds.top + row * cell;
    Some(Rect::new(left, top, left + cell, top + cell))
}

/// What a point hits: a colour index, or the swap swatch's side.
fn hit(bounds: Rect, dpi: u32, point: Point) -> (Option<usize>, Option<Side>) {
    let cell = cell_px(dpi);
    if point.x >= bounds.left && point.x < bounds.left + cell && point.y >= bounds.top {
        let half = (bounds.top + cell / 2).max(bounds.top + 1);
        let side = if point.y < half {
            Side::Primary
        } else {
            Side::Secondary
        };
        return (None, Some(side));
    }
    let index = PALETTE.iter().enumerate().find_map(|(index, _)| {
        color_rect(bounds, dpi, index)?
            .contains(point)
            .then_some(index)
    });
    (index, None)
}

/// Draws the swap swatches and the colour grid.
#[allow(clippy::too_many_arguments)]
fn paint(
    canvas: &mut dyn Canvas,
    bounds: Rect,
    dpi: u32,
    primary: Pixel,
    secondary: Pixel,
    hover: Option<usize>,
    swap_hover: Option<Side>,
    theme: &xui_core::Theme,
) {
    let cell = cell_px(dpi);
    look::band(canvas, theme);
    canvas.push_clip(bounds);

    let top = Rect::new(
        bounds.left,
        bounds.top,
        bounds.left + cell,
        bounds.top + cell / 2,
    );
    let bottom = Rect::new(
        bounds.left,
        bounds.top + cell / 2,
        bounds.left + cell,
        bounds.top + cell,
    );
    canvas.fill_rect(top.shrink(2), color_of(primary));
    canvas.fill_rect(bottom.shrink(2), color_of(secondary));
    let ring = |hovered: bool| if hovered { theme.accent } else { theme.border };
    canvas.stroke_rect(top.shrink(2), ring(swap_hover == Some(Side::Primary)), 1.0);
    canvas.stroke_rect(
        bottom.shrink(2),
        ring(swap_hover == Some(Side::Secondary)),
        1.0,
    );

    for (index, color) in PALETTE.iter().enumerate() {
        let Some(rect) = color_rect(bounds, dpi, index) else {
            continue;
        };
        if rect.left >= bounds.right || rect.top >= bounds.bottom {
            continue;
        }
        canvas.fill_rect(rect.shrink(2), color_of(*color));
        let border = if hover == Some(index) {
            theme.accent
        } else {
            theme.border
        };
        canvas.stroke_rect(rect.shrink(2), border, 1.0);
    }
    canvas.pop_clip();
}

#[cfg(test)]
mod tests;
