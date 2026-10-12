//! The window's chrome: a toolbar of icon buttons with the address field, the
//! page, and a status bar. Plain on purpose (LazyWeb's is the richer model).

use std::rc::Rc;

use xui_core::arrange::{
    build, button, column, edit, label, row, Align, Build, Handle, Layout, LayoutExt,
};
use xui_core::icon::Lucide;
use xui_core::layout::Insets;
use xui_core::widget::{Button, Edit, Label};
use xui_core::Dip;

use crate::app::Msg;
use crate::engine::Engine;
use crate::view::PageView;

const TOOLBAR_HEIGHT: Dip = Dip(38.0);
const STATUS_HEIGHT: Dip = Dip(24.0);
const TOOL_SIDE: Dip = Dip(30.0);

/// The widgets the window changes after building.
#[derive(Default)]
pub struct Widgets {
    pub page: Handle<PageView>,
    pub address: Handle<Edit<Msg>>,
    pub back: Handle<Button<Msg>>,
    pub forward: Handle<Button<Msg>>,
    /// Reload while idle, Stop while loading.
    pub reload: Handle<Button<Msg>>,
    pub status: Handle<Label<Msg>>,
}

/// The whole window.
pub fn window(widgets: &Widgets, engine: Rc<dyn Engine>) -> Layout<Msg> {
    column().children((
        toolbar(widgets).fixed(TOOLBAR_HEIGHT),
        build(move |ui| PageView::new(ui, engine))
            .bind(&widgets.page)
            .fill(1),
        status_bar(widgets).fixed(STATUS_HEIGHT),
    ))
}

fn tool(icon: Lucide, tip: &str, msg: Msg) -> Build<Button<Msg>, Msg> {
    button("").icon(icon).tooltip(tip).on_click(msg)
}

/// Back, Forward and Reload/Stop, the address field and Go.
fn toolbar(widgets: &Widgets) -> Layout<Msg> {
    row()
        .gap(6)
        .align(Align::Center)
        .padding(Insets::symmetric(Dip(6.0), Dip(4.0)))
        .children((
            row().gap(2).align(Align::Center).children((
                tool(Lucide::ChevronLeft, "Back (Alt+Left)", Msg::Back)
                    .bind(&widgets.back)
                    .width(TOOL_SIDE),
                tool(Lucide::ChevronRight, "Forward (Alt+Right)", Msg::Forward)
                    .bind(&widgets.forward)
                    .width(TOOL_SIDE),
                tool(Lucide::RefreshCw, "Reload (F5)", Msg::ReloadOrStop)
                    .bind(&widgets.reload)
                    .width(TOOL_SIDE),
            )),
            edit()
                .bind(&widgets.address)
                .placeholder("Type an address and press Enter")
                .on_change(|_| Msg::AddressEdited)
                .fill(1),
            tool(Lucide::ExternalLink, "Go (Enter)", Msg::Go).width(TOOL_SIDE),
        ))
}

fn status_bar(widgets: &Widgets) -> Layout<Msg> {
    row()
        .gap(8)
        .align(Align::Center)
        .padding(Insets::new(Dip(8.0), Dip(2.0), Dip(8.0), Dip(2.0)))
        .children((label("").bind(&widgets.status).fill(1),))
}
