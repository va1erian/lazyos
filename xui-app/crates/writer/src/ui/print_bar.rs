#![forbid(unsafe_code)]

//! The print bar: a row above the status bar, shown by Print (Ctrl+P), with
//! the printer's address, copies, page range, colour and quality, the Print
//! and Close buttons and the job's status. Paper and orientation come from
//! Page setup.

use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::arrange::{Entry, Layout, LayoutExt, column, row, spacer};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Insets;
use xui_core::widget::{Button, ComboBox, Edit, HasText, Label, NumberField};

use crate::app::Msg;
use crate::print::{COLORS, Options, QUALITIES};

/// The most copies the bar offers (the DeskJet 3700 takes 1 to 99).
const MAX_COPIES: f64 = 99.0;

/// The print bar's widgets.
pub struct PrintBar {
    labels: [Rc<Label<Msg>>; 3],
    pub printer: Rc<Edit<Msg>>,
    pub copies: Rc<NumberField<Msg>>,
    pub pages: Rc<Edit<Msg>>,
    pub color: Rc<ComboBox<Msg>>,
    pub quality: Rc<ComboBox<Msg>>,
    pub print: Rc<Button<Msg>>,
    pub close: Rc<Button<Msg>>,
    pub status: Rc<Label<Msg>>,
}

impl PrintBar {
    /// Builds the bar, hidden.
    pub fn build(ui: &Ui<Msg>) -> Result<PrintBar> {
        let label = |text: &str| Label::auto(ui, text).map(Rc::new);
        let bar = PrintBar {
            labels: [label("Printer")?, label("Copies")?, label("Pages")?],
            printer: Rc::new(Edit::auto(ui, "")?.cue("192.168.1.89")),
            copies: Rc::new(NumberField::auto(ui, 1.0, MAX_COPIES, 1.0)?),
            pages: Rc::new(Edit::auto(ui, "")?.cue("All")),
            color: Rc::new(ComboBox::auto(ui, &COLORS)?),
            quality: Rc::new(ComboBox::auto(ui, &QUALITIES.map(|(name, _)| name))?),
            print: Rc::new(Button::auto(ui, "Print")?.on_click(|| Some(Msg::PrintStart))),
            close: Rc::new(Button::auto(ui, "Close")?.on_click(|| Some(Msg::PrintClose))),
            status: label("")?,
        };
        bar.copies.set_value(1.0);
        bar.quality.select(1);
        for id in bar.ids() {
            ui.set_visible(id, false);
        }
        Ok(bar)
    }

    /// The bar as a row of the window's layout.
    pub fn row(&self) -> Layout<Msg> {
        row()
            .spacing(Dip(4.0))
            .margins(Insets::symmetric(Dip(8.0), Dip(3.0)))
            .child(middle(&self.labels[0]).width(Dip(44.0)))
            .child((&self.printer).width(Dip(150.0)))
            .child(middle(&self.labels[1]).width(Dip(42.0)))
            .child((&self.copies).width(Dip(56.0)))
            .child(middle(&self.labels[2]).width(Dip(38.0)))
            .child((&self.pages).width(Dip(72.0)))
            .child((&self.color).width(Dip(84.0)))
            .child((&self.quality).width(Dip(84.0)))
            .child((&self.print).width(Dip(64.0)))
            .child((&self.close).width(Dip(64.0)))
            .child(middle(&self.status).fill(1))
    }

    fn ids(&self) -> Vec<WidgetId> {
        let mut ids: Vec<WidgetId> = self.labels.iter().map(|l| l.id()).collect();
        ids.extend([
            self.printer.id(),
            self.copies.id(),
            self.pages.id(),
            self.color.id(),
            self.quality.id(),
            self.print.id(),
            self.close.id(),
            self.status.id(),
        ]);
        ids
    }

    /// Whether the bar is shown.
    pub fn is_shown(&self, ui: &Ui<Msg>) -> bool {
        ui.is_visible(self.print.id())
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
            printer: self.printer.text().trim().to_owned(),
            copies: self.copies.value().round().clamp(1.0, MAX_COPIES) as u32,
            pages: self.pages.text(),
            grey: self.color.selected() == 1,
            quality: QUALITIES
                .get(self.quality.selected())
                .map_or(4, |(_, quality)| *quality),
        }
    }

    /// While a job runs the choices are locked and Close cancels it.
    pub fn set_busy(&self, busy: bool) {
        self.copies.set_enabled(!busy);
        self.color.set_enabled(!busy);
        self.quality.set_enabled(!busy);
        self.print.set_enabled(!busy);
        self.close.set_text(if busy { "Cancel" } else { "Close" });
    }

    /// The status text after the buttons.
    pub fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }
}

/// A label centred on the row's height (a body label draws at its top).
fn middle(label: &Rc<Label<Msg>>) -> Entry<Msg> {
    column()
        .child(spacer())
        .child(label.height(Dip(16.0)))
        .child(spacer())
        .fill(1)
}
