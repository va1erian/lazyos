#![forbid(unsafe_code)]

//! Image > Canvas Size: a new canvas size with the picture pinned to one of
//! nine anchors, the new area transparent or the secondary colour.
//!
//! The anchor grid is nine toggle buttons of which exactly one is down; the
//! app puts the others up when one is clicked ([`CanvasSizeDialog::pick`]).

use std::cell::Cell;

use xui_core::app::Ui;
use xui_core::arrange::{
    GroupBuild, Handle, Layout, LayoutExt, column, group, label, radio_group, row, toggle_button,
};
use xui_core::backend::WidgetId;
use xui_core::layout::Align;
use xui_core::widget::{Button, Edit, GroupBox, HasText, Label, RadioGroup, ToggleButton};

use super::{button_row, field_row, parse_side, show_radio};
use crate::model::{Anchor, CanvasFill, Pixel};
use crate::view::Msg;

/// A message from the Canvas Size dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasSizeMsg {
    /// The width field changed.
    Width,
    /// The height field changed.
    Height,
    /// An anchor cell was clicked.
    Anchor(Anchor),
    /// The new-area choice changed.
    Fill,
    /// OK.
    Ok,
    /// Cancel.
    Cancel,
}

/// The Canvas Size dialog's widgets.
pub(in crate::view) struct CanvasSizeDialog {
    frame: Handle<GroupBox<Msg>>,
    width: Handle<Edit<Msg>>,
    height: Handle<Edit<Msg>>,
    cells: [Handle<ToggleButton<Msg>>; 9],
    fill: Handle<RadioGroup<Msg>>,
    secondary: Handle<Label<Msg>>,
    summary: Handle<Label<Msg>>,
    ok: Handle<Button<Msg>>,
    cancel: Handle<Button<Msg>>,
    anchor: Cell<Anchor>,
    base: Cell<(u32, u32)>,
}

impl CanvasSizeDialog {
    pub(in crate::view) fn new() -> CanvasSizeDialog {
        CanvasSizeDialog {
            frame: Handle::new(),
            width: Handle::new(),
            height: Handle::new(),
            cells: std::array::from_fn(|_| Handle::new()),
            fill: Handle::new(),
            secondary: Handle::new(),
            summary: Handle::new(),
            ok: Handle::new(),
            cancel: Handle::new(),
            anchor: Cell::new(Anchor::Center),
            base: Cell::new((1, 1)),
        }
    }

    /// The three rows of the anchor grid.
    fn grid(&self) -> Layout<Msg> {
        let rows: Vec<_> = Anchor::ALL
            .chunks(3)
            .enumerate()
            .map(|(r, anchors)| {
                let cells: Vec<_> = anchors
                    .iter()
                    .enumerate()
                    .map(|(c, &anchor)| {
                        toggle_button("")
                            .on_toggle(move |_| Msg::CanvasSize(CanvasSizeMsg::Anchor(anchor)))
                            .bind(&self.cells[r * 3 + c])
                            .width(40)
                    })
                    .collect();
                row().gap(4).children(cells).fixed(34.0)
            })
            .collect();
        column().gap(4).children(rows)
    }

    /// The frame and its content, for the layout to place over the canvas.
    pub(in crate::view) fn layout(&self) -> GroupBuild<Msg> {
        let content: Layout<Msg> = column().gap(8).padding(10).children((
            field_row(
                "Width:",
                "px",
                &self.width,
                Msg::CanvasSize(CanvasSizeMsg::Width),
                70.0,
            ),
            field_row(
                "Height:",
                "px",
                &self.height,
                Msg::CanvasSize(CanvasSizeMsg::Height),
                70.0,
            ),
            row()
                .gap(8)
                .align(Align::Start)
                .children((
                    label("Anchor:").width(70),
                    self.grid().width(140),
                    label("The picture stays at the chosen cell.")
                        .caption()
                        .fill(1),
                ))
                .fixed(114.0),
            row()
                .gap(8)
                .align(Align::Start)
                .children((
                    label("New area:").width(70),
                    radio_group(&["Transparent", "Secondary colour"])
                        .on_select(|_| Msg::CanvasSize(CanvasSizeMsg::Fill))
                        .bind(&self.fill)
                        .width(150),
                    label("")
                        .caption()
                        .bind(&self.secondary)
                        .align_y(Align::End)
                        .fill(1),
                ))
                .fixed(56.0),
            label("").bind(&self.summary).fixed(24.0),
            button_row(
                "Pixels beyond a smaller canvas are cropped.",
                (Msg::CanvasSize(CanvasSizeMsg::Ok), &self.ok),
                (Msg::CanvasSize(CanvasSizeMsg::Cancel), &self.cancel),
            ),
        ));
        group("Canvas Size", content).bind(&self.frame)
    }

    /// Hides the frame; the app calls it once after mounting.
    pub(in crate::view) fn hide_initially(&self, ui: &Ui<Msg>) {
        self.close(ui);
    }

    /// Whether the dialog is showing.
    pub(in crate::view) fn is_open(&self, ui: &Ui<Msg>) -> bool {
        self.frame
            .try_get()
            .is_some_and(|frame| ui.is_visible(frame.id()))
    }

    /// Shows the dialog for a picture of `size`, the centre anchor chosen and
    /// `secondary` named as the fill colour.
    pub(in crate::view) fn open(&self, ui: &Ui<Msg>, size: (u32, u32), secondary: Pixel) {
        self.base.set(size);
        self.width.get().set_text(&size.0.to_string());
        self.height.get().set_text(&size.1.to_string());
        self.fill.get().select(0);
        self.secondary.get().set_text(&format!(
            "= #{:02X}{:02X}{:02X}",
            secondary[0], secondary[1], secondary[2]
        ));
        self.pick(Anchor::Center);
        self.refresh_summary();
        show_radio(ui, &self.fill, true);
        ui.set_visible(self.frame.get().id(), true);
        ui.relayout();
        self.width.get().focus();
    }

    /// Hides the dialog.
    pub(in crate::view) fn close(&self, ui: &Ui<Msg>) {
        show_radio(ui, &self.fill, false);
        ui.set_visible(self.frame.get().id(), false);
    }

    /// Makes `anchor` the one cell that is down.
    pub(in crate::view) fn pick(&self, anchor: Anchor) {
        self.anchor.set(anchor);
        for (cell, &each) in self.cells.iter().zip(Anchor::ALL.iter()) {
            cell.get().set_checked(each == anchor);
        }
        self.refresh_summary();
    }

    /// A field or choice changed.
    pub(in crate::view) fn edited(&self) {
        self.refresh_summary();
    }

    fn refresh_summary(&self) {
        let text = match self.read() {
            Ok((w, h, _, _)) => {
                let (old_w, old_h) = self.base.get();
                format!("{old_w} x {old_h}  becomes  {w} x {h}")
            }
            Err(error) => error,
        };
        self.summary.get().set_text(&text);
    }

    /// The canvas size, anchor and fill the controls ask for, or why they
    /// cannot be used.
    pub(in crate::view) fn read(&self) -> Result<(u32, u32, Anchor, CanvasFill), String> {
        let width = parse_side(&self.width.get().text(), "Width")?;
        let height = parse_side(&self.height.get().text(), "Height")?;
        let fill = if self.fill.get().selected() == 0 {
            CanvasFill::Transparent
        } else {
            CanvasFill::Secondary
        };
        Ok((width, height, self.anchor.get(), fill))
    }

    /// The controls' nodes with their probe names.
    pub(in crate::view) fn ids(&self) -> Option<Vec<(String, WidgetId)>> {
        let mut ids = vec![
            ("canvas_width".to_string(), self.width.try_get()?.id()),
            ("canvas_height".to_string(), self.height.try_get()?.id()),
            (
                "canvas_fill".to_string(),
                self.fill.try_get()?.ids().first().copied()?,
            ),
            ("canvas_ok".to_string(), self.ok.try_get()?.id()),
            ("canvas_cancel".to_string(), self.cancel.try_get()?.id()),
        ];
        for (index, cell) in self.cells.iter().enumerate() {
            ids.push((format!("anchor:{index}"), cell.try_get()?.id()));
        }
        Some(ids)
    }
}
