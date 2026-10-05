//! Layout entries for the widgets xui-core's layouts cannot place by
//! themselves yet: the radio group, the colour picker and panel, the tree view
//! and the plain containers (`Panel`, a painted `Control`).
//!
//! [`placed`] creates one at a design size and wraps it in [`Placed`], which
//! gives it the [`Placeable`] a layout needs, with that size as its natural
//! size.
//!
//! One file shared by the Settings and Config crates, which include it with
//! `#[path]` rather than each keeping a copy.

use xui_core::app::Ui;
use xui_core::arrange::{build, Build};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{ColorPanel, ColorPicker, Control, Panel, Placeable, RadioGroup, TreeView};
use xui_core::Dip;

/// A widget a layout can place.
pub struct Placed<W> {
    /// The widget.
    pub widget: W,
    /// Its natural size in design pixels.
    natural: (i32, i32),
}

impl<W> Placed<W> {
    /// The natural size at `dpi`, in device pixels.
    fn natural(&self, dpi: u32) -> Size {
        device_size(self.natural, dpi)
    }
}

/// `(width, height)` design pixels at `dpi`, in device pixels.
fn device_size((width, height): (i32, i32), dpi: u32) -> Size {
    let px = |value: i32| Dip(value as f32).to_px(dpi).value();
    Size::new(px(width), px(height))
}

/// A builder for the widget `make` creates at `width` x `height` design
/// pixels, which is also its natural size in a layout.
pub fn placed<W: 'static, M: 'static>(
    width: i32,
    height: i32,
    make: impl FnOnce(&Ui<M>, Rect) -> Result<W> + 'static,
) -> Build<Placed<W>, M> {
    build(move |ui| {
        let natural = (width, height);
        let bounds = Rect::from_size(device_size(natural, ui.dpi()));
        Ok(Placed {
            widget: make(ui, bounds)?,
            natural,
        })
    })
}

/// Implements [`Placeable`] for a single-node widget.
macro_rules! single_node {
    ($($widget:ident),*) => {$(
        impl<M: 'static> Placeable<M> for Placed<$widget<M>> {
            fn id(&self) -> WidgetId {
                self.widget.id()
            }

            fn measure(&self, _ui: &Ui<M>, constraints: Constraints) -> Size {
                self.natural(constraints.dpi)
            }
        }
    )*};
}
single_node!(ColorPicker, TreeView, Panel, Control);

impl<M: 'static> Placeable<M> for Placed<ColorPanel<M>> {
    fn id(&self) -> WidgetId {
        self.widget.id()
    }

    fn measure(&self, _ui: &Ui<M>, constraints: Constraints) -> Size {
        self.natural(constraints.dpi)
    }

    fn placed(&self, _ui: &Ui<M>, rect: Rect) {
        // Its field, slider and boxes are laid out from its bounds.
        self.widget.set_bounds(rect);
    }
}

/// A radio group is one node per option: the layout places the first, and
/// [`Placeable::placed`] stacks every option in the rectangle it was given.
impl<M: 'static> Placeable<M> for Placed<RadioGroup<M>> {
    fn id(&self) -> WidgetId {
        self.widget.ids().first().copied().unwrap_or(WidgetId::NONE)
    }

    fn measure(&self, _ui: &Ui<M>, constraints: Constraints) -> Size {
        self.natural(constraints.dpi)
    }

    fn placed(&self, ui: &Ui<M>, rect: Rect) {
        let ids = self.widget.ids();
        let row = rect.height() / (ids.len().max(1) as i32);
        let moves: Vec<(WidgetId, Rect)> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let top = rect.top + row * index as i32;
                (*id, Rect::new(rect.left, top, rect.right, top + row))
            })
            .collect();
        ui.apply_moves(&moves);
    }
}
