#![forbid(unsafe_code)]

//! The print bar: a row above the status bar, shown by Print (Ctrl+P), with
//! the printer's address, copies, page range, colour and quality, the Print
//! and Close buttons and the job's status. Paper and orientation come from
//! Page setup.

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::arrange::{
    Align, Build, Entry, Handle, Layout, LayoutExt, button, combo_box, edit, label, number_field,
    row,
};
use xui_core::backend::WidgetId;
use xui_core::layout::Insets;
use xui_core::widget::{Button, ComboBox, Edit, HasText, Label, NumberField, Placeable};

use crate::app::Msg;
use crate::print::{COLORS, Options, QUALITIES};

/// The most copies the bar offers (the DeskJet 3700 takes 1 to 99).
const MAX_COPIES: f64 = 99.0;

/// The print bar's widgets, filled when its row is mounted.
#[derive(Default)]
pub struct PrintBar {
    labels: [Handle<Label<Msg>>; 3],
    pub printer: Handle<Edit<Msg>>,
    copies: Handle<NumberField<Msg>>,
    pages: Handle<Edit<Msg>>,
    color: Handle<ComboBox<Msg>>,
    quality: Handle<ComboBox<Msg>>,
    print: Handle<Button<Msg>>,
    close: Handle<Button<Msg>>,
    pub status: Handle<Label<Msg>>,
}

impl PrintBar {
    /// The bar as a row of the window's layout, hidden until Print.
    pub fn row(&self) -> Layout<Msg> {
        let [printer_label, copies_label, pages_label] = &self.labels;
        let qualities = QUALITIES.map(|(name, _)| name);
        row()
            .gap(4)
            .padding(Insets::symmetric(Dip(8.0), Dip(3.0)))
            .children((
                middle(label("Printer").bind(printer_label)).width(44),
                hidden(edit().placeholder("192.168.1.89").bind(&self.printer)).width(150),
                middle(label("Copies").bind(copies_label)).width(42),
                hidden(
                    number_field(1.0, MAX_COPIES, 1.0)
                        .bind(&self.copies)
                        .then(|copies| {
                            copies.set_value(1.0);
                            copies
                        }),
                )
                .width(56),
                middle(label("Pages").bind(pages_label)).width(38),
                hidden(edit().placeholder("All").bind(&self.pages)).width(72),
                hidden(combo_box(&COLORS).bind(&self.color)).width(84),
                hidden(combo_box(&qualities).bind(&self.quality).then(|quality| {
                    quality.select(1);
                    quality
                }))
                .width(84),
                hidden(
                    button("Print")
                        .bind(&self.print)
                        .on_click_with(|| Some(Msg::PrintStart)),
                )
                .width(64),
                hidden(
                    button("Close")
                        .bind(&self.close)
                        .on_click_with(|| Some(Msg::PrintClose)),
                )
                .width(64),
                middle(label("").bind(&self.status)).fill(1),
            ))
    }

    fn ids(&self) -> Vec<WidgetId> {
        let mut ids: Vec<WidgetId> = self.labels.iter().map(|l| l.get().id()).collect();
        ids.extend([
            self.printer.get().id(),
            self.copies.get().id(),
            self.pages.get().id(),
            self.color.get().id(),
            self.quality.get().id(),
            self.print.get().id(),
            self.close.get().id(),
            self.status.get().id(),
        ]);
        ids
    }

    /// Whether the bar is shown.
    pub fn is_shown(&self, ui: &Ui<Msg>) -> bool {
        ui.is_visible(self.print.get().id())
    }

    /// Shows or hides the bar; the window's layout re-flows around it.
    pub fn set_shown(&self, ui: &Ui<Msg>, shown: bool) {
        for id in self.ids() {
            ui.set_visible(id, shown);
        }
    }

    /// The choices as they stand.
    pub fn options(&self) -> Options {
        Options {
            printer: self.printer.get().text().trim().to_owned(),
            copies: self.copies.get().value().round().clamp(1.0, MAX_COPIES) as u32,
            pages: self.pages.get().text(),
            grey: self.color.get().selected() == 1,
            quality: QUALITIES
                .get(self.quality.get().selected())
                .map_or(4, |(_, quality)| *quality),
        }
    }

    /// While a job runs the choices are locked and Close cancels it.
    pub fn set_busy(&self, busy: bool) {
        self.copies.get().set_enabled(!busy);
        self.color.get().set_enabled(!busy);
        self.quality.get().set_enabled(!busy);
        self.print.get().set_enabled(!busy);
        self.close
            .get()
            .set_text(if busy { "Cancel" } else { "Close" });
    }

    /// The status text after the buttons.
    pub fn set_status(&self, text: &str) {
        self.status.get().set_text(text);
    }
}

/// A label centred on the row's height, hidden (a body label draws its text
/// at its top, so it is capped to one line's height before centring).
fn middle(label: Build<Label<Msg>, Msg>) -> Entry<Msg> {
    hidden(label).align(Align::Center).max_height(16)
}

/// Creates the widget hidden: the bar shows on Print.
fn hidden<W: Placeable<Msg>>(widget: Build<W, Msg>) -> Build<W, Msg> {
    widget.then_with(|widget, ui| {
        ui.set_visible(widget.id(), false);
        Ok(widget)
    })
}
