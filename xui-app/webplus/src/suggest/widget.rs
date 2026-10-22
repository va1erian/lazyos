//! The dropdown as a widget: a column of text rows over the page, the highlighted one filled. It
//! is custom painted because it has to be exactly as tall as its rows, which a list view cannot be.
//! The keyboard belongs to the address field; this takes the pointer only (hover, click).

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{
    Event, NodeKind, NodeSpec, Result, TextAlign, TextStyle, TextVAlign, WidgetId,
};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Dip, MouseButton};

use super::dropdown::Act;
use crate::app::Msg;

/// A row's height, the box's width and the text's size and inset, in design units.
const ROW: Dip = Dip(24.0);
const WIDTH: Dip = Dip(560.0);
const TEXT: Dip = Dip(13.0);
const INSET: Dip = Dip(10.0);

#[derive(Default)]
struct View {
    rows: Vec<String>,
    selected: Option<usize>,
    hover: Option<usize>,
}

pub struct SuggestBox {
    control: Control<Msg>,
    view: Rc<RefCell<View>>,
}

impl SuggestBox {
    pub fn new(ui: &Ui<Msg>) -> Result<SuggestBox> {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))?;
        let view = Rc::new(RefCell::new(View::default()));
        {
            let view = Rc::clone(&view);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                let theme = theme.get();
                let bounds = canvas.bounds();
                canvas.fill_rect(bounds, theme.raised);
                let row = ROW.to_px(canvas.dpi()).value();
                let inset = INSET.to_px(canvas.dpi()).value();
                let view = view.borrow();
                for (n, text) in view.rows.iter().enumerate() {
                    let top = bounds.top + n as i32 * row;
                    let cell = Rect::new(bounds.left, top, bounds.right, top + row);
                    if view.selected == Some(n) {
                        canvas.fill_rect(cell, theme.selection);
                    } else if view.hover == Some(n) {
                        canvas.fill_rect(cell, theme.hover);
                    }
                    let mut style = TextStyle::new(theme.text, TEXT);
                    style.valign = TextVAlign::Middle;
                    style.align = TextAlign::Start;
                    let text_rect =
                        Rect::new(cell.left + inset, cell.top, cell.right - inset, cell.bottom);
                    canvas.draw_text(text, text_rect, &style);
                }
                canvas.stroke_rect(bounds, theme.border, 1.0);
            }));
        }
        {
            let (view, ui) = (Rc::clone(&view), ui.clone());
            let id = control.id();
            control.on_events(move |event| {
                let row = ROW.to_px(ui.dpi()).value().max(1);
                match event {
                    Event::MouseMove { y, .. } => {
                        let at = row_at(&view.borrow(), *y, row);
                        if view.borrow().hover != at {
                            view.borrow_mut().hover = at;
                            ui.invalidate(id);
                        }
                        None
                    }
                    Event::MouseLeave => {
                        view.borrow_mut().hover = None;
                        ui.invalidate(id);
                        None
                    }
                    Event::MouseDown {
                        y,
                        button: MouseButton::Left,
                        ..
                    } => row_at(&view.borrow(), *y, row).map(|n| Msg::Suggest(Act::Pick(n))),
                    _ => None,
                }
            });
        }
        Ok(SuggestBox { control, view })
    }

    /// Shows `rows`, with row `selected` highlighted.
    pub fn set(&self, rows: Vec<String>, selected: Option<usize>) {
        {
            let mut view = self.view.borrow_mut();
            view.rows = rows;
            view.selected = selected;
            view.hover = None;
        }
        self.control.invalidate_layout();
    }

    /// Moves the highlight.
    pub fn select(&self, selected: Option<usize>) {
        self.view.borrow_mut().selected = selected;
        self.control.invalidate();
    }

    /// Where row `n` is, for a session script.
    pub fn row_rect(&self, n: usize, bounds: Rect, dpi: u32) -> Rect {
        let row = ROW.to_px(dpi).value();
        let top = bounds.top + n as i32 * row;
        Rect::new(bounds.left, top, bounds.right, top + row)
    }
}

fn row_at(view: &View, y: i32, row: i32) -> Option<usize> {
    let n = usize::try_from(y / row).ok().filter(|_| y >= 0)?;
    (n < view.rows.len()).then_some(n)
}

impl Placeable<Msg> for SuggestBox {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// As tall as its rows; nothing at all without rows.
    fn measure(&self, ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        let rows = self.view.borrow().rows.len() as i32;
        if rows == 0 {
            return Size::new(0, 0);
        }
        let dpi = ui.dpi();
        Size::new(WIDTH.to_px(dpi).value(), rows * ROW.to_px(dpi).value())
    }

    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        self.control.set_bounds(rect);
    }
}
