#![forbid(unsafe_code)]

//! The colour editor: xui's [`ColorPanel`] (swatches, an HSV field, hex/RGB/CMYK
//! entry) in a titled frame that takes the canvas's place while a colour is
//! edited.
//!
//! A frame in the window (rather than a modal dialog; xui gap: G17) because xui's `Dialog`
//! holds only text and an entry, and a second window cannot be driven by the
//! offscreen test backend. The frame shares a `stack` with the canvas, so the app
//! shows one and hides the other ([`ColorEditor::open`], [`ColorEditor::close`]).
//!
//! Changes reach the app live ([`Msg::ColorEditing`]), so the swatch follows the
//! pointer; OK keeps the colour and Cancel restores the one the edit started
//! from. The panel carries no alpha: the model's colours are opaque until the
//! layers phase (P3) adds it.

use std::rc::Rc;

use xui_core::Color;
use xui_core::app::Ui;
use xui_core::arrange::{GroupBuild, Layout};
use xui_core::arrange::{Handle, LayoutExt, button, color_panel, column, group, label, row};
use xui_core::backend::WidgetId;
use xui_core::layout::Align;
use xui_core::widget::{Button, ColorPanel, GroupBox};

use super::{Msg, color_of};
use crate::model::{Pixel, Side};

/// The editor's widgets.
pub(super) struct ColorEditor {
    frame: Handle<GroupBox<Msg>>,
    panel: Handle<ColorPanel<Msg>>,
    ok: Handle<Button<Msg>>,
    cancel: Handle<Button<Msg>>,
}

/// An opaque pixel from a panel colour.
fn pixel_of(color: Color) -> Pixel {
    [color.r, color.g, color.b, 255]
}

impl ColorEditor {
    pub(super) fn new() -> ColorEditor {
        ColorEditor {
            frame: Handle::new(),
            panel: Handle::new(),
            ok: Handle::new(),
            cancel: Handle::new(),
        }
    }

    /// The frame and its content, for the layout to place over the canvas.
    pub(super) fn layout(&self) -> GroupBuild<Msg> {
        let content: Layout<Msg> = column().gap(8).padding(8).children((
            color_panel()
                .then(|panel| {
                    panel
                        .on_change(|color| Some(Msg::ColorEditing(pixel_of(color))))
                        .on_commit(|color| Some(Msg::ColorEditing(pixel_of(color))))
                })
                .bind(&self.panel)
                .fill(1),
            row()
                .gap(8)
                .align(Align::Center)
                .children((
                    label("Changes apply as you edit.").caption().fill(1),
                    button("OK")
                        .on_click(Msg::ColorDone)
                        .primary()
                        .bind(&self.ok)
                        .width(80),
                    button("Cancel")
                        .on_click(Msg::ColorCancel)
                        .bind(&self.cancel)
                        .width(80),
                ))
                .fixed(34),
        ));
        group("Edit colour", content).bind(&self.frame)
    }

    /// Hides the frame; the app calls it once after mounting.
    pub(super) fn hide_initially(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.get().id(), false);
    }

    /// Shows the editor on `colour`, titled for `side`.
    pub(super) fn open(&self, ui: &Ui<Msg>, side: Side, colour: Pixel) {
        let frame = self.frame.get();
        frame.set_title(match side {
            Side::Primary => "Edit primary colour",
            Side::Secondary => "Edit secondary colour",
        });
        self.panel.get().set_color(color_of(colour));
        self.panel.get().select_tab(1);
        ui.set_visible(frame.id(), true);
        ui.relayout();
    }

    /// Hides the editor.
    pub(super) fn close(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.get().id(), false);
    }

    /// Whether the editor is showing.
    pub(super) fn is_open(&self, ui: &Ui<Msg>) -> bool {
        self.frame
            .try_get()
            .is_some_and(|frame| ui.is_visible(frame.id()))
    }

    /// The OK and Cancel buttons, for the UI probe.
    pub(super) fn button_ids(&self) -> Option<(WidgetId, WidgetId)> {
        Some((self.ok.try_get()?.id(), self.cancel.try_get()?.id()))
    }

    /// The panel, for tests.
    pub(super) fn panel(&self) -> Rc<ColorPanel<Msg>> {
        self.panel.get()
    }
}
