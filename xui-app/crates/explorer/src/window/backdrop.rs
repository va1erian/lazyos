#![forbid(unsafe_code)]

//! Click-away for the window's menus. xui's in-window menus close only on a
//! choice or Escape, and a click on empty toolbar space or on the status bar
//! reaches no widget that raises a message. An invisible [`Backdrop`] under
//! the whole layout takes the clicks that land between widgets, and the
//! status bar gets a mapper of its own; both raise [`Msg::Dismiss`], which
//! `update` turns into closing the menus.

use xui_core::app::Ui;
use xui_core::arrange::{Build, build};
use xui_core::backend::{Event, NodeKind, NodeSpec, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};

use super::Msg;

/// An empty node the layout stretches under everything else.
pub(super) struct Backdrop {
    control: Control<Msg>,
}

impl Placeable<Msg> for Backdrop {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// No size of its own: the stack it sits in stretches it.
    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

/// The backdrop, raising [`Msg::Dismiss`] for a press on it.
pub(super) fn backdrop() -> Build<Backdrop, Msg> {
    build(|ui| {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Container, Rect::default()))?;
        control.on_events(dismiss);
        Ok(Backdrop { control })
    })
}

/// [`Msg::Dismiss`] for any mouse press on a node with no use for it.
pub(super) fn dismiss(event: &Event) -> Option<Msg> {
    matches!(event, Event::MouseDown { .. }).then_some(Msg::Dismiss)
}
