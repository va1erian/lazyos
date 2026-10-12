#![forbid(unsafe_code)]

//! The Colors panel: the primary/secondary pair with swap and reset, and a
//! 32-colour palette.
//!
//! `ColorPicker` reports only a chosen [`Color`](xui_core::Color) and ignores
//! which button was used, so this custom node maps left/right clicks on a swatch
//! to the primary/secondary colour. A left click on the primary or secondary
//! swatch asks to edit it ([`Msg::EditColor`]); the editor is
//! [`super::coloredit`].

use std::cell::Cell;
use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::backend::{Canvas, Event, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::icon::{Lucide, draw_icon};
use xui_core::layout::Constraints;
use xui_core::message::MouseButton;
use xui_core::theme::look;
use xui_core::widget::{Control, Placeable};

use super::{Msg, color_of};
use crate::model::{Pixel, Side};

/// Padding around the panel's content.
const PAD: Dip = Dip(8.0);
/// One palette cell.
const CELL: Dip = Dip(18.0);
/// One side of a primary/secondary swatch.
const SWATCH: Dip = Dip(30.0);
/// How far the secondary swatch sits down and right of the primary.
const OFFSET: Dip = Dip(14.0);
/// One side of the swap and reset buttons.
const BUTTON: Dip = Dip(22.0);

/// The number of palette colours.
pub const COLORS: usize = 32;

/// The 32 palette colours, eight to a row: the 16 classic paint colours first,
/// then tints and extra hues.
pub const PALETTE: [Pixel; COLORS] = [
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
    [64, 64, 64, 255],
    [192, 192, 192, 255],
    [255, 160, 192, 255],
    [255, 128, 128, 255],
    [255, 204, 0, 255],
    [255, 255, 128, 255],
    [128, 255, 128, 255],
    [160, 255, 208, 255],
    [128, 200, 255, 255],
    [128, 128, 255, 255],
    [75, 0, 130, 255],
    [200, 160, 255, 255],
    [160, 64, 255, 255],
    [255, 128, 192, 255],
    [210, 180, 140, 255],
    [64, 32, 0, 255],
];

/// What a point of the panel hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    /// The primary swatch.
    Primary,
    /// The secondary swatch.
    Secondary,
    /// The swap button.
    Swap,
    /// The reset button.
    Reset,
    /// A palette colour.
    Color(usize),
}

/// The panel's rectangles for a given placed rect and DPI.
struct Geometry {
    bounds: Rect,
    dpi: u32,
}

impl Geometry {
    fn px(&self, dip: Dip) -> i32 {
        dip.to_px(self.dpi).value().max(1)
    }

    fn square(&self, left: i32, top: i32, side: Dip) -> Rect {
        let side = self.px(side);
        Rect::new(
            self.bounds.left + left,
            self.bounds.top + top,
            self.bounds.left + left + side,
            self.bounds.top + top + side,
        )
    }

    /// The primary swatch, in front.
    fn primary(&self) -> Rect {
        self.square(self.px(PAD), self.px(PAD), SWATCH)
    }

    /// The secondary swatch, behind and down-right of the primary.
    fn secondary(&self) -> Rect {
        let at = self.px(PAD) + self.px(OFFSET);
        self.square(at, at, SWATCH)
    }

    /// The swap button, right of the swatches at the top.
    fn swap(&self) -> Rect {
        let left = self.px(PAD) + self.px(OFFSET) + self.px(SWATCH) + self.px(PAD);
        self.square(left, self.px(PAD), BUTTON)
    }

    /// The reset button, below the swap button.
    fn reset(&self) -> Rect {
        let swap = self.swap();
        let gap = self.px(PAD) / 2;
        Rect::new(
            swap.left,
            swap.bottom + gap,
            swap.right,
            swap.bottom + gap + self.px(BUTTON),
        )
    }

    /// The bottom of the header: the secondary swatch plus padding.
    fn header_bottom(&self) -> i32 {
        self.secondary().bottom + self.px(PAD)
    }

    fn columns(&self) -> i32 {
        ((self.bounds.width() - 2 * self.px(PAD)) / self.px(CELL)).max(1)
    }

    /// Palette colour `index`'s cell.
    fn color(&self, index: usize) -> Option<Rect> {
        if index >= COLORS {
            return None;
        }
        let cell = self.px(CELL);
        let columns = self.columns();
        let row = index as i32 / columns;
        let column = index as i32 % columns;
        let left = self.bounds.left + self.px(PAD) + column * cell;
        let top = self.header_bottom() + row * cell;
        Some(Rect::new(left, top, left + cell, top + cell))
    }

    /// The height the panel needs at its width.
    fn height(&self) -> i32 {
        let rows = (COLORS as i32 + self.columns() - 1) / self.columns();
        self.header_bottom() - self.bounds.top + rows * self.px(CELL) + self.px(PAD)
    }

    /// What `point` hits. The primary swatch is in front of the secondary.
    fn hit(&self, point: Point) -> Option<Hit> {
        let fixed = [
            (self.primary(), Hit::Primary),
            (self.secondary(), Hit::Secondary),
            (self.swap(), Hit::Swap),
            (self.reset(), Hit::Reset),
        ];
        if let Some((_, hit)) = fixed.into_iter().find(|(rect, _)| rect.contains(point)) {
            return Some(hit);
        }
        (0..COLORS).find_map(|index| {
            self.color(index)
                .filter(|rect| rect.contains(point))
                .map(|_| Hit::Color(index))
        })
    }
}

/// The panel's shared state.
struct State {
    bounds: Cell<Rect>,
    dpi: Cell<u32>,
    primary: Cell<Pixel>,
    secondary: Cell<Pixel>,
    hover: Cell<Option<Hit>>,
}

impl State {
    fn geometry(&self) -> Geometry {
        Geometry {
            bounds: self.bounds.get(),
            dpi: self.dpi.get(),
        }
    }
}

// xui gap: G9 — `ColorPicker` reports only a colour and ignores the button, so
// left/right cannot choose the primary/secondary swatch.
/// The Colors panel node.
pub struct Palette {
    control: Control<Msg>,
    state: Rc<State>,
}

impl Palette {
    /// Creates the panel; the window layout places it.
    pub fn new(ui: &Ui<Msg>) -> Result<Palette> {
        let control = Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, Rect::default()).tab_stop(),
        )?;
        let state = Rc::new(State {
            bounds: Cell::new(Rect::default()),
            dpi: Cell::new(ui.dpi()),
            primary: Cell::new(PALETTE[0]),
            secondary: Cell::new(PALETTE[15]),
            hover: Cell::new(None),
        });

        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                paint(canvas, &state, &theme.get());
            }));
        }
        {
            let state = Rc::clone(&state);
            let ui = ui.clone();
            let id = control.id();
            control.on_events(move |event| handle(&ui, id, &state, event));
        }

        Ok(Palette { control, state })
    }

    /// The panel's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Refreshes the shown primary/secondary colours.
    pub fn sync(&self, primary: Pixel, secondary: Pixel) {
        self.state.primary.set(primary);
        self.state.secondary.set(secondary);
        self.control.invalidate();
    }

    /// The device-pixel rect of palette colour `index`, for tests.
    pub fn color_rect(&self, index: usize) -> Option<Rect> {
        self.state.geometry().color(index)
    }

    /// The device-pixel rect of a swatch that a click reaches, for tests and
    /// the UI probe: the primary whole, and the secondary's strip right of the
    /// primary (the rest of it is behind the primary).
    pub fn swatch_rect(&self, side: Side) -> Rect {
        let geometry = self.state.geometry();
        let (primary, secondary) = (geometry.primary(), geometry.secondary());
        match side {
            Side::Primary => primary,
            Side::Secondary => Rect::new(
                primary.right,
                secondary.top,
                secondary.right,
                secondary.bottom,
            ),
        }
    }

    /// The device-pixel rect of the swap button.
    pub fn swap_rect(&self) -> Rect {
        self.state.geometry().swap()
    }

    /// The device-pixel rect of the reset button.
    pub fn reset_rect(&self) -> Rect {
        self.state.geometry().reset()
    }

    /// Shows or hides the panel.
    pub fn set_visible(&self, visible: bool) {
        self.control.set_visible(visible);
    }
}

impl Placeable<Msg> for Palette {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// As tall as the header and the colour rows need at the offered width.
    fn measure(&self, _ui: &Ui<Msg>, constraints: Constraints) -> Size {
        let width = constraints.max_width.unwrap_or(i32::MAX / 2);
        let geometry = Geometry {
            bounds: Rect::new(0, 0, width, 0),
            dpi: constraints.dpi,
        };
        Size::new(0, geometry.height())
    }

    /// Hit-testing and the grid follow the placed rect.
    fn placed(&self, ui: &Ui<Msg>, rect: Rect) {
        self.state.bounds.set(rect);
        self.state.dpi.set(ui.dpi());
    }
}

/// The panel's event mapper.
fn handle(ui: &Ui<Msg>, id: WidgetId, state: &Rc<State>, event: &Event) -> Option<Msg> {
    let bounds = state.bounds.get();
    let at = |x: i32, y: i32| Point::new(bounds.left + x, bounds.top + y);
    match event {
        Event::MouseMove { x, y, .. } => {
            let hit = state.geometry().hit(at(*x, *y));
            if state.hover.get() != hit {
                state.hover.set(hit);
                ui.invalidate(id);
            }
            None
        }
        Event::MouseLeave | Event::CaptureChanged => {
            state.hover.set(None);
            ui.invalidate(id);
            None
        }
        Event::MouseDown { x, y, button, .. } => {
            let side = match button {
                MouseButton::Left => Side::Primary,
                MouseButton::Right => Side::Secondary,
                _ => return None,
            };
            match state.geometry().hit(at(*x, *y))? {
                Hit::Primary | Hit::Secondary if side == Side::Secondary => None,
                Hit::Primary => Some(Msg::EditColor(Side::Primary)),
                Hit::Secondary => Some(Msg::EditColor(Side::Secondary)),
                Hit::Swap => (side == Side::Primary).then_some(Msg::SwapColors),
                Hit::Reset => (side == Side::Primary).then_some(Msg::ResetColors),
                Hit::Color(index) => Some(Msg::Palette {
                    color: PALETTE[index],
                    side,
                }),
            }
        }
        _ => None,
    }
}

/// Draws the swatches, their buttons and the colour grid.
fn paint(canvas: &mut dyn Canvas, state: &State, theme: &xui_core::Theme) {
    let geometry = state.geometry();
    let hover = state.hover.get();
    let ring = |hit: Hit| {
        if hover == Some(hit) {
            theme.accent
        } else {
            theme.border
        }
    };
    look::band(canvas, theme);
    canvas.push_clip(geometry.bounds);

    for (hit, rect, color) in [
        (Hit::Secondary, geometry.secondary(), state.secondary.get()),
        (Hit::Primary, geometry.primary(), state.primary.get()),
    ] {
        fill_swatch(canvas, rect, color);
        canvas.stroke_rect(rect, ring(hit), 1.0);
    }

    let hot = |hit: Hit, rect: Rect, canvas: &mut dyn Canvas| {
        if hover == Some(hit) {
            canvas.fill_rounded_rect(rect, 3.0, theme.hover);
        }
    };
    let swap = geometry.swap();
    hot(Hit::Swap, swap, canvas);
    draw_icon(canvas, Lucide::Repeat, swap, theme.text, geometry.dpi);
    let reset = geometry.reset();
    hot(Hit::Reset, reset, canvas);
    draw_reset(canvas, reset, theme);

    for (index, color) in PALETTE.iter().enumerate() {
        let Some(rect) = geometry.color(index) else {
            continue;
        };
        canvas.fill_rect(rect.shrink(1), color_of(*color));
        canvas.stroke_rect(rect.shrink(1), ring(Hit::Color(index)), 1.0);
    }
    canvas.pop_clip();
}

/// Fills `rect` with `color`; one that is not opaque is shown over a
/// checkerboard, mixed cell by cell, so its alpha can be seen.
fn fill_swatch(canvas: &mut dyn Canvas, rect: Rect, color: Pixel) {
    if color[3] == 255 {
        canvas.fill_rect(rect, color_of(color));
        return;
    }
    let cell = (rect.width() / 4).max(2);
    let mut y = rect.top;
    while y < rect.bottom {
        let mut x = rect.left;
        while x < rect.right {
            let dark = ((x - rect.left) / cell + (y - rect.top) / cell) % 2 == 1;
            let grey = if dark { 204 } else { 255 };
            let under = [grey, grey, grey, 255];
            let part = Rect::new(
                x,
                y,
                (x + cell).min(rect.right),
                (y + cell).min(rect.bottom),
            );
            canvas.fill_rect(part, color_of(crate::model::source_over(under, color, 255)));
            x += cell;
        }
        y += cell;
    }
}

/// The reset glyph: a small black square in front of a white one, the colours
/// `D` restores.
fn draw_reset(canvas: &mut dyn Canvas, rect: Rect, theme: &xui_core::Theme) {
    let side = (rect.width() * 5 / 11).max(3);
    let back = Rect::new(
        rect.right - side - rect.width() / 6,
        rect.bottom - side - rect.height() / 6,
        rect.right - rect.width() / 6,
        rect.bottom - rect.height() / 6,
    );
    let front = Rect::new(
        rect.left + rect.width() / 6,
        rect.top + rect.height() / 6,
        rect.left + rect.width() / 6 + side,
        rect.top + rect.height() / 6 + side,
    );
    canvas.fill_rect(back, color_of(crate::model::WHITE));
    canvas.stroke_rect(back, theme.border, 1.0);
    canvas.fill_rect(front, color_of([0, 0, 0, 255]));
}

#[cfg(test)]
mod tests;
