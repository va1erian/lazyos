//! The Windows page: one of the themed colours, picked from a list, edited
//! with xui's full [`ColorPanel`] (HSV field, hue slider, HEX/RGB boxes).

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, color_panel, column, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::ColorPanel;
use xui_core::Color;

use crate::app::{choice_list, Msg};
use crate::appearance_page::pack;

/// The page's widgets.
pub struct WindowsPage {
    pub panel: Rc<ColorPanel<Msg>>,
    _mounted: Mounted<Msg>,
}

impl WindowsPage {
    /// Lays the page out in the container `page`, listing the colours by
    /// `names` with the first selected.
    pub fn build(ui: &Ui<Msg>, page: WidgetId, names: Vec<&'static str>) -> Result<WindowsPage> {
        let panel = Handle::new();
        let mounted = ui.mount_in(
            page,
            row().padding(20).gap(16).children((
                column()
                    .gap(8)
                    .children((
                        label("Color to change"),
                        // The panel opens on target 0, the list's first row.
                        choice_list(&names).on_select(Msg::Target).height(150),
                        button("Use default")
                            .on_click(Msg::UseDefault)
                            .align(Align::Start),
                    ))
                    .width(160),
                color_panel()
                    .then(|panel| panel.on_commit(|c| Some(Msg::Commit(pack(c)))))
                    .bind(&panel)
                    .fill(1)
                    .max_height(360),
            )),
        )?;
        Ok(WindowsPage {
            panel: panel.get(),
            _mounted: mounted,
        })
    }

    /// Points the colour panel at `rgb`.
    pub fn set_color(&self, rgb: u32) {
        self.panel.set_color(Color::hex(rgb));
    }
}
