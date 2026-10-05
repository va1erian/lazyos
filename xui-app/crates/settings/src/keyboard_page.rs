//! The Keyboard page: the layout `inputd` follows ([`keyboard`]) and a field
//! to try it in.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{build, column, edit, label, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Label, ListView};
use xui_core::{HasText, Rect};

use crate::app::Msg;
use crate::keyboard;

/// The widest the list and the test field get.
const FIELD_W: i32 = 300;

/// The page's widgets.
pub struct KeyboardPage {
    layout: Rc<ListView<Msg>>,
    hint: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
}

impl KeyboardPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<KeyboardPage> {
        let (layout, hint) = (Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(20).gap(8).children((
                label("Keyboard layout"),
                build(|ui| {
                    let names: Vec<&str> = keyboard::LAYOUTS.iter().map(|(_, n)| *n).collect();
                    Ok(ListView::new(ui, Rect::default(), &names)?
                        .multi_select(false)
                        .on_select(|i| Some(Msg::Layout(i))))
                })
                .bind(&layout)
                .height(60)
                .max_width(FIELD_W),
                label("").bind(&hint),
                label("Try it"),
                edit()
                    .placeholder("Type here to test the layout")
                    .max_width(FIELD_W),
            )),
        )?;
        Ok(KeyboardPage {
            layout: layout.get(),
            hint: hint.get(),
            _mounted: mounted,
        })
    }

    /// Selects layout `index` (no event is raised) and names it.
    pub fn show_layout(&self, index: usize) {
        self.layout.select(Some(index));
        self.show_active(index);
    }

    /// Names layout `index` as the active one.
    pub fn show_active(&self, index: usize) {
        if let Some((_, name)) = keyboard::LAYOUTS.get(index) {
            self.hint.set_text(&format!("Active: {name}"));
        }
    }

    /// Says no layout was chosen yet.
    pub fn show_default(&self) {
        self.hint
            .set_text("No layout chosen yet (using the boot default).");
    }
}
