//! The Config window's widgets: the two panes' layouts and the handles the
//! app mirrors its state onto.

use xui_core::arrange::{
    build, button, column, edit, label, row, spacer, Build, Handle, Layout, LayoutExt,
};
use xui_core::layout::{Align, Insets};
use xui_core::widget::{Button, Edit, Label, ListView, Panel, RadioGroup};
use xui_core::{Dip, Rect};

use crate::app::Msg;
use crate::place::{placed, Placed};
use crate::value_edit::Kind;

/// Height of a kind radio group: one 28-dip row per kind.
const KINDS_H: i32 = 28 * Kind::ALL.len() as i32;

/// The widgets the app changes after building.
#[derive(Default)]
pub struct Widgets {
    pub filter: Handle<Edit<Msg>>,
    pub list: Handle<ListView<Msg>>,
    pub path: Handle<Label<Msg>>,
    pub kind: Handle<Placed<RadioGroup<Msg>>>,
    pub value: Handle<Edit<Msg>>,
    pub bool_button: Handle<Button<Msg>>,
    pub apply: Handle<Button<Msg>>,
    pub revert: Handle<Button<Msg>>,
    pub delete: Handle<Button<Msg>>,
    pub preview: Handle<Label<Msg>>,
    pub new_toggle: Handle<Button<Msg>>,
    pub new_path: Handle<Edit<Msg>>,
    pub new_kind: Handle<Placed<RadioGroup<Msg>>>,
    pub new_value: Handle<Edit<Msg>>,
    pub create: Handle<Button<Msg>>,
    pub banner: Handle<Label<Msg>>,
    pub status: Handle<Label<Msg>>,
}

/// A kind radio group raising `msg` with the picked index.
fn kinds(msg: fn(usize) -> Msg) -> Build<Placed<RadioGroup<Msg>>, Msg> {
    placed(140, KINDS_H, move |ui, bounds| {
        let labels: Vec<&str> = Kind::ALL.iter().map(|kind| kind.label()).collect();
        Ok(RadioGroup::new(ui, bounds, &labels)?.on_select(move |index| Some(msg(index))))
    })
}

/// A card for a pane, laid out by the window.
pub fn card() -> Build<Placed<Panel<Msg>>, Msg> {
    placed(0, 0, Panel::new)
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
            build(|ui| {
                Ok(ListView::new(ui, Rect::default(), &[])?
                    .multi_select(false)
                    .on_select(|index| Some(Msg::Select(index))))
            })
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
                kinds(Msg::Kind).bind(&self.kind).align(Align::Start),
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
                    kinds(Msg::NewKind).bind(&self.new_kind).align(Align::Start),
                )),
                spacer(),
                label("").bind(&self.banner),
            ))
    }
}
