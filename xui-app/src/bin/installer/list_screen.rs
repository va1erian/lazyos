//! The list screen: the installed applications, a `Remove` button per row, and
//! the "open a package" field.
//!
//! The rows live in a [`ScrollView`] rebuilt from the model, so a list of any
//! length scrolls instead of overflowing; an empty list says so in words.

use xui_core::app::Ui;
use xui_core::units::Dip;
use xui_core::widget::{Button, Edit, Label, Panel, ScrollView};

use xui_app::installer::{elide, Model};

use crate::msg::Msg;
use crate::view::{fail, rect, MARGIN};

/// The height of one installed-app row.
const ROW_H: i32 = 40;
/// The width reserved for the scrollbar so a row's Remove button never sits
/// under it.
const BAR_RESERVE: i32 = 14;
/// The bottom strip holding the path field and the status banner.
const BOTTOM_H: i32 = 96;

/// One installed-app row: the panel that owns it and its child widgets.
struct AppRow {
    _panel: Panel<Msg>,
    _name: Label<Msg>,
    _meta: Label<Msg>,
    _remove: Button<Msg>,
}

/// The installed-list screen's widgets.
pub struct ListScreen {
    _panel: Panel<Msg>,
    _title: Label<Msg>,
    _empty: Label<Msg>,
    _scroll: ScrollView<Msg>,
    _rows: Vec<AppRow>,
    _path: Edit<Msg>,
    _inspect: Button<Msg>,
    _reload: Button<Msg>,
    _banner: Label<Msg>,
}

impl ListScreen {
    /// Builds the screen at `width` x `height` from `model`.
    pub fn build(
        ui: &Ui<Msg>,
        width: i32,
        height: i32,
        model: &Model,
    ) -> Result<ListScreen, String> {
        let panel = Panel::new(ui, rect(0, 0, width, height)).map_err(fail)?;
        let page = panel.ui();
        let title = Label::new(
            page,
            rect(MARGIN, 10, width - 2 * MARGIN, 20),
            "Installed applications",
        )
        .map_err(fail)?;

        let scroll_top = 36;
        let scroll_h = (height - BOTTOM_H - scroll_top).max(40);
        let scroll = ScrollView::new(page, rect(MARGIN, scroll_top, width - 2 * MARGIN, scroll_h))
            .map_err(fail)?;

        // The row width is the viewport width minus the scrollbar reserve; the
        // scroll view re-lays each row's own bounds on top of this.
        let row_w = (width - 2 * MARGIN - BAR_RESERVE).max(120);
        let mut rows = Vec::new();
        {
            let scoped = scroll.ui();
            for app in &model.packages {
                let row = Panel::new(scoped, rect(0, 0, row_w, ROW_H)).map_err(fail)?;
                let cell = row.ui();
                let name = Label::new(cell, rect(8, 4, row_w - 112, 18), &elide(&app.name, 48))
                    .map_err(fail)?;
                let meta = Label::new(
                    cell,
                    rect(8, 20, row_w - 112, 14),
                    &format!(
                        "v{}  ·  {}",
                        elide(&app.version, 20),
                        elide(&app.system_name, 48)
                    ),
                )
                .map_err(fail)?;
                let system_name = app.system_name.clone();
                let remove = Button::new(cell, rect(row_w - 96, 6, 88, 28), "Remove")
                    .map_err(fail)?
                    .on_click(move || Some(Msg::AskRemove(system_name.clone())));
                scroll.add(row.id(), Dip(ROW_H as f32));
                rows.push(AppRow {
                    _panel: row,
                    _name: name,
                    _meta: meta,
                    _remove: remove,
                });
            }
        }

        let empty = Label::new(
            page,
            rect(MARGIN + 8, scroll_top + 12, width - 2 * MARGIN - 16, 18),
            "No applications are installed yet. Open a package below to add one.",
        )
        .map_err(fail)?;
        ui.set_visible(empty.id(), model.packages.is_empty());
        ui.raise(empty.id());

        let edit_w = (width - 2 * MARGIN - 210).max(80);
        let path = Edit::new(
            page,
            rect(MARGIN, height - 84, edit_w, 26),
            &model.path_input,
        )
        .map_err(fail)?
        .cue("Absolute path to a .lzp package")
        .on_change(|text| Some(Msg::PathChanged(text.to_owned())));
        // The field is where a keyboard user starts: focus it so typing a path
        // works without a click, and so a pointer click on it is not needed to
        // leave the last-built button.
        path.focus();
        let inspect = Button::new(
            page,
            rect(MARGIN + edit_w + 8, height - 84, 96, 26),
            "Inspect",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::Inspect));
        let reload = Button::new(
            page,
            rect(MARGIN + edit_w + 112, height - 84, 96, 26),
            "Refresh",
        )
        .map_err(fail)?
        .on_click(|| Some(Msg::Reload));

        let banner_text = model.banner.as_deref().unwrap_or("");
        let banner = Label::new(
            page,
            rect(MARGIN, height - 50, width - 2 * MARGIN, 18),
            &elide(banner_text, 160),
        )
        .map_err(fail)?;

        Ok(ListScreen {
            _panel: panel,
            _title: title,
            _empty: empty,
            _scroll: scroll,
            _rows: rows,
            _path: path,
            _inspect: inspect,
            _reload: reload,
            _banner: banner,
        })
    }
}
