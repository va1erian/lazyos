//! The page as a layout entry. [`BlitzView`] exposes no node of its own to
//! place, so the window layout places a container and the view fills it.

use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_blitz::BlitzView;

use crate::app::Msg;

/// The Blitz view in a frame the window layout places.
pub struct Page {
    frame: Control<Msg>,
    view: BlitzView<Msg>,
}

impl Page {
    /// Creates the frame and the view inside it, and starts loading `url`.
    pub fn new(ui: &Ui<Msg>, url: &str) -> Result<Page> {
        let frame = Control::new(ui, &NodeSpec::new(NodeKind::Container, Rect::default()))?;
        // The engine starts laying the page out at the view's first size, so
        // start near the final one (the client width) rather than 1x1.
        let start = Rect::from_size(ui.client_rect().size());
        let view = BlitzView::builder(|| Msg::Frame, |event| Some(Msg::View(event)))
            .url(url)
            .follow_links(true)
            .build(&ui.with_parent(frame.id()), start)?;
        Ok(Page { frame, view })
    }

    /// The view.
    pub fn view(&self) -> &BlitzView<Msg> {
        &self.view
    }
}

impl Placeable<Msg> for Page {
    fn id(&self) -> WidgetId {
        self.frame.id()
    }

    /// The page takes whatever its `fill` entry gives it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    /// The view fills the frame; it tells the engine its new size on its
    /// next paint.
    fn placed(&self, _ui: &Ui<Msg>, rect: Rect) {
        self.view.set_bounds(Rect::from_size(rect.size()));
    }
}
