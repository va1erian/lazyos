//! The window's widgets and layout (`docs/messenger-playground-plan.md`
//! section 3): menu bar, the developer-tool strip, the Explorer, the doc pane,
//! the editor and the Output. The topic tool is the Topic monitor's own window
//! ([`crate::topicsui`]).
//!
//! [`Widgets`] holds a handle to every widget the app changes after start-up;
//! [`Widgets::layout`] builds the tree. Sizes are weights and design pixels,
//! not coordinates, so the window resizes and scales (docs/hidpi-plan.md) with
//! its layout. Two dividers (xui `Split`s) are the user's: the Explorer's width
//! and the doc pane's share ([`crate::panes`], [`crate::panesapp`]).

use std::rc::Rc;

use xui_app::devtool::{strip, Notice, Strip};
use xui_code_editor::{Editor, FontConfig, Options, RhaiHighlighter};

use xui_core::app::Ui;
use xui_core::arrange::{
    build as create, button, column, edit, label, list, panel, row, split, status_bar, tabs,
    tree_view, Build, Handle, Layout, LayoutExt,
};
use xui_core::backend::{Result, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{
    Button, Edit, Label, ListView, Menu, Panel, Placeable, ScrollView, Split, StatusBar, Tabs,
    TreeView,
};
use xui_core::{Dip, Fill};

use crate::menus::{menus, MENU_HEIGHT};
use crate::msg::{Action, Msg};
use crate::panes::{self, Saved};
use crate::paste::{PasteTap, TappedClipboard};

/// The editor as a layout entry: it takes all the leftover space.
pub struct EditorPane(pub Rc<Editor<Msg>>);

impl Placeable<Msg> for EditorPane {
    fn id(&self) -> WidgetId {
        self.0.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }

    fn placed(&self, _ui: &Ui<Msg>, _rect: Rect) {
        self.0.on_resize();
    }
}

/// The Rhai code editor, in the monospace face.
///
/// Its clipboard is tapped, so the window knows when an edit was a paste
/// (`crate::paste`).
fn new_editor(ui: &Ui<Msg>, tap: PasteTap) -> Result<EditorPane> {
    let options = Options {
        font: FontConfig {
            family: Some(xui_app::font::MONO_FAMILY.to_owned()),
            ..FontConfig::default()
        },
        ..Options::default()
    };
    let editor = Editor::with_options(ui, Rect::default(), options)?
        .with_highlighter(RhaiHighlighter)
        .with_clipboard(TappedClipboard::new(
            xui_code_editor::platform::clipboard(ui),
            tap,
        ))
        .on_change(|text| Some(Msg::Edited(text.to_string())));
    Ok(EditorPane(Rc::new(editor)))
}

/// The buttons that act on the selected node.
#[derive(Default)]
pub struct ActionButtons {
    pub insert_call: Handle<Button<Msg>>,
    pub call_form: Handle<Button<Msg>>,
    pub serve: Handle<Button<Msg>>,
    pub connect: Handle<Button<Msg>>,
    pub listen: Handle<Button<Msg>>,
    pub peek: Handle<Button<Msg>>,
    pub publish: Handle<Button<Msg>>,
    pub insert_on: Handle<Button<Msg>>,
    pub load: Handle<Button<Msg>>,
}

impl ActionButtons {
    /// `(probe name, id)` of each, in reading order.
    pub fn all(&self) -> [(&'static str, WidgetId); 9] {
        [
            ("insert_call", self.insert_call.get().id()),
            ("call_form", self.call_form.get().id()),
            ("serve", self.serve.get().id()),
            ("connect", self.connect.get().id()),
            ("listen", self.listen.get().id()),
            ("peek", self.peek.get().id()),
            ("publish", self.publish.get().id()),
            ("insert_on", self.insert_on.get().id()),
            ("load", self.load.get().id()),
        ]
    }
}

/// Every widget the app changes after start-up.
#[derive(Default)]
pub struct Widgets {
    /// Set when the editor reads the clipboard (a paste).
    pub paste: PasteTap,
    pub menu: Handle<Menu<Msg>>,
    pub strip: Handle<Strip<Msg>>,
    pub search: Handle<Edit<Msg>>,
    pub explorer: Handle<Tabs<Msg>>,
    pub tree: Handle<TreeView<Msg>>,
    pub live: Handle<ListView<Msg>>,
    pub topics: Handle<ListView<Msg>>,
    pub gallery: Handle<ListView<Msg>>,
    pub doc: Handle<Panel<Msg>>,
    /// The scroll view of the page the doc pane shows now.
    pub doc_scroll: Handle<ScrollView<Msg>>,
    /// Explorer | right column, and doc pane | bottom row.
    pub split_main: Handle<Split<Msg>>,
    pub split_doc: Handle<Split<Msg>>,
    /// The editor and the Output, shown and hidden as one.
    pub bottom: Handle<Panel<Msg>>,
    pub toggle_bottom: Handle<Button<Msg>>,
    pub actions: ActionButtons,
    pub form_call: Handle<Button<Msg>>,
    pub form_insert: Handle<Button<Msg>>,
    pub form_close: Handle<Button<Msg>>,
    pub editor: Handle<EditorPane>,
    pub file: Handle<Label<Msg>>,
    pub run: Handle<Button<Msg>>,
    pub stop: Handle<Button<Msg>>,
    pub output: Handle<Tabs<Msg>>,
    pub status: Handle<StatusBar<Msg>>,
}

fn action(
    text: &str,
    action: Action,
    handle: &Handle<Button<Msg>>,
) -> xui_core::arrange::Entry<Msg> {
    button(text)
        .on_click(Msg::Do(action))
        .bind(handle)
        .into_entry_sized()
}

trait Sized2 {
    fn into_entry_sized(self) -> xui_core::arrange::Entry<Msg>;
}

impl Sized2 for Build<Button<Msg>, Msg> {
    fn into_entry_sized(self) -> xui_core::arrange::Entry<Msg> {
        xui_core::arrange::IntoEntry::into_entry(self)
    }
}

impl Widgets {
    /// The whole window, its dividers where `saved` puts them.
    pub fn layout(&self, notice: &Rc<Notice>, saved: &Saved) -> Layout<Msg> {
        column().gap(0).children((
            menus().bind(&self.menu).height(MENU_HEIGHT),
            strip(Rc::clone(notice), Msg::NoticeDismiss).bind(&self.strip),
            // The Explorer the whole height at a width of its own (the window's extra
            // width goes to the right column), and on its right the doc pane over
            // the editor and the Output. The topic tool is its own window.
            row()
                .padding(6)
                .children((split(self.explorer_column(), self.right_column(saved))
                    .position(Dip(saved.explorer))
                    .min(Dip(panes::EXPLORER_MIN), Dip(panes::RIGHT_MIN))
                    .on_moved(Msg::ExplorerMoved)
                    .bind(&self.split_main)
                    .fill(1),))
                .fill(1),
            status_bar(&["Ready", "", ""]).bind(&self.status),
        ))
    }

    /// The doc pane over the editor and the Output, a divider between them
    /// (the app sets its position from the share, [`crate::panesapp`]).
    fn right_column(&self, _saved: &Saved) -> Layout<Msg> {
        column().child(
            split(
                self.doc_column(),
                column().child(
                    panel(
                        row().gap(8).children((
                            self.editor_column().fill(56),
                            self.output_column().fill(44),
                        )),
                    )
                    .plain()
                    .bind(&self.bottom)
                    .fill(1),
                ),
            )
            .stacked()
            .position(Dip(300.0))
            .min(Dip(panes::DOC_MIN), Dip(panes::BOTTOM_MIN))
            .on_moved(Msg::DocSplitMoved)
            .bind(&self.split_doc)
            .fill(1),
        )
    }

    fn explorer_column(&self) -> Layout<Msg> {
        column().gap(4).children((
            row().gap(4).children((
                edit()
                    .placeholder("Search interfaces, methods, fields")
                    .on_change(Msg::Search)
                    .bind(&self.search)
                    .fill(1),
                button("Refresh").on_click(Msg::TopicsRefresh),
            )),
            tabs()
                .page(
                    "Interfaces",
                    column().child(
                        tree_view()
                            .on_select(Msg::TreeSelect)
                            .then(|t| t.on_toggle(|id, open| Some(Msg::TreeToggle(id, open))))
                            .bind(&self.tree)
                            .fill(1),
                    ),
                )
                .page(
                    "Live",
                    column().child(
                        list()
                            .column("Name", 120)
                            .column("Interfaces", 150)
                            .column("uid", 40)
                            .column("label", 50)
                            .column("session", 60)
                            .column("lease", Fill)
                            .on_select(Msg::LiveSelect)
                            .on_activate(Msg::LiveActivate)
                            .bind(&self.live)
                            .fill(1),
                    ),
                )
                .page(
                    "Topics",
                    column().child(
                        list()
                            .column("Topic", 150)
                            .column("Payload", 80)
                            .column("QoS", 60)
                            .column("Ret", 36)
                            .column("Subs", 40)
                            .column("Permissions", Fill)
                            .on_select(Msg::TopicSelect)
                            .bind(&self.topics)
                            .fill(1),
                    ),
                )
                .page(
                    "Gallery",
                    column().child(
                        list()
                            .column("#", 36)
                            .column("Example", 170)
                            .column("Needs", Fill)
                            .on_select(Msg::GallerySelect)
                            .on_activate(Msg::GalleryActivate)
                            .bind(&self.gallery)
                            .fill(1),
                    ),
                )
                .on_change(|i| Msg::ExplorerTab(i))
                .bind(&self.explorer)
                .fill(1),
        ))
    }

    fn doc_column(&self) -> Layout<Msg> {
        let a = &self.actions;
        column().gap(4).children((
            row().gap(4).children((
                action("Insert call", Action::InsertCall, &a.insert_call),
                action("Call form", Action::CallForm, &a.call_form),
                action("Serve skeleton", Action::ServeSkeleton, &a.serve),
                action("Connect", Action::Connect, &a.connect),
            )),
            row().gap(4).children((
                action("Listen", Action::Listen, &a.listen),
                action("Peek", Action::Peek, &a.peek),
                action("Publish", Action::Publish, &a.publish),
                action("Insert on_", Action::InsertOn, &a.insert_on),
                action("Load example", Action::LoadExample, &a.load),
            )),
            panel(column()).plain().bind(&self.doc).fill(1),
            row().gap(4).children((
                label("Call form:").align(xui_core::arrange::Align::Center),
                button("Call").on_click(Msg::FormCall).bind(&self.form_call),
                button("Insert")
                    .on_click(Msg::FormInsert)
                    .bind(&self.form_insert),
                button("Close")
                    .on_click(Msg::FormClose)
                    .bind(&self.form_close),
                label("").fill(1),
                button("Hide editor")
                    .on_click(Msg::ToggleBottom)
                    .bind(&self.toggle_bottom),
            )),
        ))
    }

    fn editor_column(&self) -> Layout<Msg> {
        column().gap(4).children((
            row()
                .gap(6)
                .align(xui_core::arrange::Align::Center)
                .children((
                    label("script.rhai").bind(&self.file).fill(1),
                    button("Run  F5")
                        .on_click(Msg::Run)
                        .bind(&self.run)
                        .primary(),
                    button("Stop").on_click(Msg::Stop).bind(&self.stop),
                    button("Gallery").on_click(Msg::ExplorerTab(3)),
                )),
            {
                let tap = self.paste.clone();
                create(move |ui| new_editor(ui, tap))
                    .bind(&self.editor)
                    .fill(1)
            },
        ))
    }

    fn output_column(&self) -> Layout<Msg> {
        column().gap(4).children((
            label("Output").caption(),
            tabs()
                .page(
                    "Output",
                    column().child(label("Runs appear here. Press Run (F5).")),
                )
                .on_change(|i| Msg::OutputTab(i))
                .bind(&self.output)
                .fill(1),
        ))
    }
}
