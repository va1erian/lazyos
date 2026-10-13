//! The sidebar's backdrop: the window darkened, with a hairline on its right edge.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::Size;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Point, Rect, Rgba};

use crate::app::Msg;

/// The sidebar's own background: the window darkened, with a hairline on its
/// right edge, so the section list reads as a column apart from the page. A
/// container, so the section list sits inside it.
pub struct Backdrop(pub Control<Msg>);

/// A painted container has no content to size it: the layout gives it its
/// width and the window's height.
impl Placeable<Msg> for Backdrop {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

pub fn sidebar_backdrop(ui: &Ui<Msg>) -> Result<Backdrop> {
    let back = Control::new(ui, &NodeSpec::new(NodeKind::Container, Rect::default()))?;
    let theme = ui.theme_handle();
    back.set_painter(Rc::new(move |canvas| {
        let theme = theme.get();
        let b = canvas.bounds();
        xui_core::theme::look::backdrop(canvas, theme.background);
        let shade = if theme.is_dark { 0x40 } else { 0x0C };
        canvas.fill_rect_rgba(b, Rgba::with_alpha(0, 0, 0, shade));
        canvas.draw_line(
            Point::new(b.right - 1, b.top),
            Point::new(b.right - 1, b.bottom),
            theme.border,
            1.0,
        );
    }));
    Ok(Backdrop(back))
}
