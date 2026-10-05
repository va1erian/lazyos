//! The Config window's widgets: the two panes' layouts and the handles the
//! app mirrors its state onto.

use xui_core::arrange::{
    button, column, edit, label, list, radio_group, row, spacer, Entry, Handle, Layout, LayoutExt,
};
use xui_core::layout::{Align, Insets};
use xui_core::widget::{Button, Edit, Label, ListView, RadioGroup};
use xui_core::Dip;

use crate::app::Msg;
use crate::value_edit::Kind;

/// Height of a kind radio group: one 28-dip row per kind.
const KINDS_H: i32 = 28 * Kind::ALL.len() as i32;

/// The widgets the app changes after building.
#[derive(Default)]
pub struct Widgets {
    pub filter: Handle<Edit<Msg>>,
    pub list: Handle<ListView<Msg>>,
    pub path: Handle<Label<Msg>>,
    pub kind: Handle<RadioGroup<Msg>>,
    pub value: Handle<Edit<Msg>>,
    pub bool_button: Handle<Button<Msg>>,
    pub apply: Handle<Button<Msg>>,
    pub revert: Handle<Button<Msg>>,
    pub delete: Handle<Button<Msg>>,
    pub preview: Handle<Label<Msg>>,
    pub new_toggle: Handle<Button<Msg>>,
    pub new_path: Handle<Edit<Msg>>,
    pub new_kind: Handle<RadioGroup<Msg>>,
    pub new_value: Handle<Edit<Msg>>,
    pub create: Handle<Button<Msg>>,
    pub banner: Handle<Label<Msg>>,
    pub status: Handle<Label<Msg>>,
}

/// A kind radio group raising `msg` with the picked index, bound to `handle`.
fn kinds(msg: fn(usize) -> Msg, handle: &Handle<RadioGroup<Msg>>) -> Entry<Msg> {
    let labels = Kind::ALL.map(Kind::label);
    radio_group(&labels)
        .on_select(msg)
        .bind(handle)
        .size(140, KINDS_H)
        .align(Align::Start)
}

impl Widgets {
    /// The tree pane: the filter and the key list.
    pub fn tree_pane(&self) -> Layout<Msg> {
        column().padding(10).gap(8).children((
            row().gap(6).children((
                edit()
                    .placeholder("filter paths")
                    .on_change(Msg::Filter)
                    .bind(&self.filter)
                    .fill(1),
                button("Refresh").on_click(Msg::Refresh),
            )),
            list()
                .then(|list| list.multi_select(false))
                .on_select(Msg::Select)
                .bind(&self.list)
                .fill(1),
        ))
    }

    /// The key pane: the selected key, then the create-key form.
    pub fn key_pane(&self) -> Layout<Msg> {
        column()
            .padding(Insets::symmetric(Dip(14.0), Dip(10.0)))
            .gap(8)
            .children((
                label("No key selected").bind(&self.path),
                kinds(Msg::Kind, &self.kind),
                // Only one of the two shows: a bool is toggled, not typed.
                row().children((
                    edit().on_change(Msg::Value).bind(&self.value).fill(1),
                    button("false")
                        .on_click(Msg::ToggleBool)
                        .bind(&self.bool_button)
                        .width(120),
                )),
                row().gap(8).children((
                    button("Apply").on_click(Msg::Apply).bind(&self.apply),
                    button("Revert").on_click(Msg::Revert).bind(&self.revert),
                    button("Delete")
                        .on_click(Msg::Delete)
                        .bind(&self.delete)
                        .width(110),
                    button("Reload").on_click(Msg::Reload),
                )),
                label("").bind(&self.preview),
                row().gap(10).children((
                    column()
                        .gap(6)
                        .children((
                            button("New key")
                                .on_click(Msg::NewToggle)
                                .bind(&self.new_toggle)
                                .align(Align::Start),
                            edit()
                                .placeholder("sys/... path")
                                .on_change(Msg::NewPath)
                                .bind(&self.new_path),
                            edit()
                                .placeholder("value")
                                .on_change(Msg::NewValue)
                                .bind(&self.new_value),
                            button("Create")
                                .on_click(Msg::Create)
                                .bind(&self.create)
                                .align(Align::Start),
                        ))
                        .fill(1),
                    kinds(Msg::NewKind, &self.new_kind),
                )),
                spacer(),
                label("").bind(&self.banner),
            ))
    }
}
