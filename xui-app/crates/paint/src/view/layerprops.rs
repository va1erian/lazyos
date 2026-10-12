#![forbid(unsafe_code)]

//! Layer Properties: name, visibility and opacity in a titled frame that takes
//! the canvas's place, like the colour editor.
//!
//! A frame in the window rather than a modal dialog for the same reason as the
//! colour editor (xui gap: G17 — `Dialog` holds only text and one entry). The
//! controls raise [`Msg::PropsChanged`]; the app reads them back with
//! [`LayerPropsDialog::read`] and shows the result on the layer live, OK keeps
//! it as one history entry and Cancel restores the original.

use xui_core::app::Ui;
use xui_core::arrange::{GroupBuild, Handle, Layout, LayoutExt};
use xui_core::arrange::{button, checkbox, column, edit, group, label, row, slider};
use xui_core::backend::WidgetId;
use xui_core::layout::Align;
use xui_core::widget::{Button, CheckBox, Edit, GroupBox, HasText, Label, Slider};

use super::Msg;
use crate::model::LayerProps;

/// The percent an opacity of `value` out of 255 shows as.
pub(super) fn percent(value: u8) -> u32 {
    (u32::from(value) * 100 + 127) / 255
}

/// The dialog's widgets.
pub(super) struct LayerPropsDialog {
    frame: Handle<GroupBox<Msg>>,
    name: Handle<Edit<Msg>>,
    visible: Handle<CheckBox<Msg>>,
    opacity: Handle<Slider<Msg>>,
    value: Handle<Label<Msg>>,
    ok: Handle<Button<Msg>>,
    cancel: Handle<Button<Msg>>,
}

impl LayerPropsDialog {
    pub(super) fn new() -> LayerPropsDialog {
        LayerPropsDialog {
            frame: Handle::new(),
            name: Handle::new(),
            visible: Handle::new(),
            opacity: Handle::new(),
            value: Handle::new(),
            ok: Handle::new(),
            cancel: Handle::new(),
        }
    }

    /// The frame and its content, for the layout to place over the canvas.
    pub(super) fn layout(&self) -> GroupBuild<Msg> {
        let content: Layout<Msg> = column().gap(10).padding(10).children((
            row()
                .gap(8)
                .align(Align::Center)
                .children((
                    label("Name:").width(70),
                    edit()
                        .on_change(|_| Msg::PropsChanged)
                        .bind(&self.name)
                        .fill(1),
                ))
                .fixed(30),
            checkbox("Visible")
                .checked(true)
                .on_toggle(|_| Msg::PropsChanged)
                .bind(&self.visible)
                .fixed(26),
            row()
                .gap(8)
                .align(Align::Center)
                .children((
                    label("Opacity:").width(70),
                    slider(0.0, 255.0)
                        .on_change(|_| Msg::PropsChanged)
                        .bind(&self.opacity)
                        .fill(1),
                    label("100%").bind(&self.value).width(48),
                ))
                .fixed(30),
            row()
                .gap(8)
                .align(Align::Center)
                .children((
                    label("Changes apply as you edit.").caption().fill(1),
                    button("OK")
                        .on_click(Msg::PropsOk)
                        .primary()
                        .bind(&self.ok)
                        .width(80),
                    button("Cancel")
                        .on_click(Msg::PropsCancel)
                        .bind(&self.cancel)
                        .width(80),
                ))
                .fixed(34),
        ));
        group("Layer Properties", content).bind(&self.frame)
    }

    /// Hides the frame; the app calls it once after mounting.
    pub(super) fn hide_initially(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.get().id(), false);
    }

    /// Shows the dialog on `props`.
    pub(super) fn open(&self, ui: &Ui<Msg>, props: &LayerProps) {
        self.name.get().set_text(&props.name);
        self.visible.get().set_checked(props.visible);
        self.opacity.get().set_value(f64::from(props.opacity));
        self.show_value(props.opacity);
        ui.set_visible(self.frame.get().id(), true);
        ui.relayout();
        self.name.get().focus();
    }

    /// Hides the dialog.
    pub(super) fn close(&self, ui: &Ui<Msg>) {
        ui.set_visible(self.frame.get().id(), false);
    }

    /// Whether the dialog is showing.
    pub(super) fn is_open(&self, ui: &Ui<Msg>) -> bool {
        self.frame
            .try_get()
            .is_some_and(|frame| ui.is_visible(frame.id()))
    }

    /// The properties the controls show now.
    pub(super) fn read(&self) -> LayerProps {
        LayerProps {
            name: self.name.get().text(),
            visible: self.visible.get().is_checked(),
            opacity: self.opacity.get().value().round().clamp(0.0, 255.0) as u8,
        }
    }

    /// Updates the opacity's percent label.
    pub(super) fn show_value(&self, opacity: u8) {
        self.value.get().set_text(&format!("{}%", percent(opacity)));
    }

    /// The controls' nodes with their probe names.
    pub(super) fn ids(&self) -> Option<[(&'static str, WidgetId); 5]> {
        Some([
            ("props_name", self.name.try_get()?.id()),
            ("props_visible", self.visible.try_get()?.id()),
            ("props_opacity", self.opacity.try_get()?.id()),
            ("props_ok", self.ok.try_get()?.id()),
            ("props_cancel", self.cancel.try_get()?.id()),
        ])
    }
}
