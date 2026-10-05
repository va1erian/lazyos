//! The display: a card with the expression line on top and the number,
//! large and right-aligned, under it.
//!
//! xui's `Label` is left-aligned at one of three sizes, so the display is
//! its own painted node. It is also where the keyboard focus rests: a
//! clicked keypad button would otherwise keep it, and Enter would press
//! that button as well as `=`.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Canvas, NodeKind, NodeSpec, Result, TextAlign, TextStyle, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::theme::look::backdrop;
use xui_core::widget::{Control, Placeable};
use xui_core::{Dip, Theme};

use crate::app::Msg;

/// The card's design height.
pub const HEIGHT: Dip = Dip(84.0);
/// The number's largest and smallest design sizes: a long number shrinks
/// to fit instead of being cut.
const NUMBER_SIZES: [f32; 5] = [36.0, 32.0, 28.0, 24.0, 20.0];
const EXPRESSION_SIZE: Dip = Dip(13.0);
const INSET: Dip = Dip(12.0);
const RADIUS: f32 = 6.0;

/// What the display shows.
#[derive(Default)]
struct Shown {
    number: String,
    expression: String,
    error: bool,
}

/// The display node.
pub struct Display {
    control: Control<Msg>,
    shown: Rc<RefCell<Shown>>,
}

impl Display {
    pub fn new(ui: &Ui<Msg>) -> Result<Display> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        let shown = Rc::new(RefCell::new(Shown {
            number: "0".to_owned(),
            ..Shown::default()
        }));
        let theme = ui.theme_handle();
        let painted = Rc::clone(&shown);
        control.set_painter(Rc::new(move |canvas| {
            paint(canvas, &painted.borrow(), &theme.get());
        }));
        Ok(Display { control, shown })
    }

    /// Shows `number` under `expression`; `error` paints it in the danger
    /// colour.
    pub fn show(&self, number: &str, expression: &str, error: bool) {
        *self.shown.borrow_mut() = Shown {
            number: number.to_owned(),
            expression: expression.to_owned(),
            error,
        };
        self.control.invalidate();
    }

    /// Takes the keyboard focus, so Enter reaches no button.
    pub fn focus(&self) {
        self.control.focus();
    }
}

impl Placeable<Msg> for Display {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The layout gives it a fixed height and the full width.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

fn paint(canvas: &mut dyn Canvas, shown: &Shown, theme: &Theme) {
    let dpi = canvas.dpi();
    let bounds = canvas.bounds();
    backdrop(canvas, theme.background);
    let card = bounds.shrink(1);
    canvas.fill_rounded_rect(card, RADIUS, theme.input_background);
    canvas.stroke_rounded_rect(card, RADIUS, theme.input_border, 1.0);

    let inner = card.shrink(INSET.to_px(dpi).value());
    let (top, bottom) = inner.split_top(inner.height() / 3);
    let mut expression = TextStyle::new(theme.text_secondary, EXPRESSION_SIZE);
    expression.align = TextAlign::End;
    canvas.draw_text(&shown.expression, top, &expression);

    let color = if shown.error {
        theme.danger
    } else {
        theme.text
    };
    let style = fitted(canvas, &shown.number, color, bottom.width());
    canvas.draw_text(&shown.number, bottom, &style);
}

/// The largest number style whose text fits `width`.
fn fitted(canvas: &dyn Canvas, text: &str, color: xui_core::Color, width: i32) -> TextStyle {
    let style = |size: f32| {
        let mut style = TextStyle::new(color, Dip(size)).middle();
        style.align = TextAlign::End;
        style
    };
    NUMBER_SIZES
        .iter()
        .map(|&size| style(size))
        .find(|style| canvas.measure_text(text, style).width <= width)
        .unwrap_or_else(|| style(NUMBER_SIZES[NUMBER_SIZES.len() - 1]))
}
