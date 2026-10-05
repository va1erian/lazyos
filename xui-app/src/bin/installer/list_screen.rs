//! The list screen: the installed applications, a `Remove` button per row (a
//! "Built-in" badge instead for the core apps LazyOS ships, which cannot be
//! removed), and the button that starts the install wizard.
//!
//! The rows live in a [`ScrollView`] rebuilt from the model, so a list of any
//! length scrolls instead of overflowing; an empty list says so in words.

use xui_core::app::Ui;
use xui_core::arrange::{build, button, column, label, row, Layout, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::{Align, Constraints, Insets};
use xui_core::units::Dip;
use xui_core::widget::{Panel, Placeable, ScrollView};

use xui_app::installer::{elide, Installed, Model};

use crate::msg::Msg;
use crate::view::MARGIN;
use crate::wizard::focused;

/// The height of one installed-app row.
const ROW_H: f32 = 52.0;

/// The installed apps: a scroll view of one card per app, each laid out by
/// its own layout.
struct AppList {
    scroll: ScrollView<Msg>,
    rows: Vec<(Panel<Msg>, Mounted<Msg>)>,
}

impl AppList {
    fn build(ui: &Ui<Msg>, apps: &[Installed]) -> Result<AppList> {
        let scroll = ScrollView::new(ui, Rect::default())?;
        let mut rows = Vec::new();
        for app in apps {
            let panel = Panel::new(scroll.ui(), Rect::default())?;
            scroll.add(panel.id(), Dip(ROW_H));
            let mounted = scroll.ui().mount_in(panel.id(), app_row(app))?;
            rows.push((panel, mounted));
        }
        Ok(AppList { scroll, rows })
    }
}

impl Placeable<Msg> for AppList {
    fn id(&self) -> WidgetId {
        self.scroll.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, _rect: Rect) {
        // The view sizes its rows from its own bounds; each row then lays
        // its widgets out in its new width.
        self.scroll.relayout();
        for (_, mounted) in &self.rows {
            mounted.relayout();
        }
    }
}

/// One installed-app row: its name and version over its system name, then
/// Remove (or the badge of a core app).
fn app_row(app: &Installed) -> Layout<Msg> {
    let action = if app.core {
        label("Built-in").align(Align::Center)
    } else {
        let system_name = app.system_name.clone();
        button("Remove")
            .on_click(Msg::AskRemove(system_name))
            .width(88)
            .align(Align::Center)
    };
    row()
        .padding(Insets::symmetric(Dip(8.0), Dip(0.0)))
        .gap(8)
        .children((
            column()
                .justify(Align::Center)
                .children((
                    label(elide(&app.name, 48)),
                    label(format!(
                        "v{}  ·  {}",
                        elide(&app.version, 20),
                        elide(&app.system_name, 48)
                    )),
                ))
                .fill(1),
            action,
        ))
}

/// The installed list, its actions and the status banner.
pub fn layout(model: &Model) -> Layout<Msg> {
    let apps = model.packages.clone();
    let list = if apps.is_empty() {
        row()
            .padding(Insets::symmetric(Dip(8.0), Dip(12.0)))
            .child(label(
                "No applications are installed yet. Install a package to add one.",
            ))
            .fill(1)
    } else {
        build(move |ui| AppList::build(ui, &apps)).fill(1)
    };
    let banner = elide(model.banner.as_deref().unwrap_or(""), 160);
    column().padding(MARGIN).gap(8).children((
        label("Installed applications"),
        list,
        row().gap(8).children((
            // The wizard is the screen's main action: Enter starts it.
            focused(button("Install a package…").on_click(Msg::StartInstall)),
            button("Refresh").on_click(Msg::Reload).width(96),
        )),
        label(banner),
    ))
}
