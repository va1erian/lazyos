//! The window's layout: a toolbar, a deck of two pages (the mail view and the
//! account page) and a status line. In the mail view the folder pane sits
//! beside a second deck: the message list with the reading pane, or the
//! compose form. A page is shown by hiding the others (`Ui::set_visible`).

use std::rc::Rc;

use xui_core::Dip;
use xui_core::arrange::{
    Align, Handle, Layout, LayoutExt, build, button, column, label, list, panel, row, stack,
};
use xui_core::icon::Lucide;
use xui_core::layout::Insets;
use xui_core::widget::{Fill, Label, ListView, Panel};

use super::Msg;
use super::account::{self, AccountWidgets};
use super::compose::{self, ComposeWidgets};
use super::reader::Reader;

/// Height of the toolbar and of the status line, in design units.
const TOOLBAR_H: f32 = 36.0;
const STATUS_H: f32 = 24.0;
/// Width of the folder pane and of the message list.
const FOLDERS_W: f32 = 190.0;
const LIST_W: f32 = 330.0;
/// Width of a toolbar button, so the four line up.
const BUTTON_W: f32 = 104.0;

/// The handles the layout fills when it is mounted.
#[derive(Default)]
pub struct Widgets {
    pub folder_list: Handle<ListView<Msg>>,
    pub message_list: Handle<ListView<Msg>>,
    pub reader: Handle<Reader>,
    pub status: Handle<Label<Msg>>,
    /// The folder pane and everything beside it.
    pub mail_view: Handle<Panel<Msg>>,
    /// The message list and the reading pane.
    pub panes: Handle<Panel<Msg>>,
    pub account: AccountWidgets,
    pub compose: ComposeWidgets,
}

/// The mounted widgets, from the filled handles.
pub struct Mounted {
    pub folder_list: Rc<ListView<Msg>>,
    pub message_list: Rc<ListView<Msg>>,
    pub reader: Rc<Reader>,
    pub status: Rc<Label<Msg>>,
    pub mail_view: Rc<Panel<Msg>>,
    pub panes: Rc<Panel<Msg>>,
}

impl Widgets {
    pub fn mounted(&self) -> Mounted {
        Mounted {
            folder_list: self.folder_list.get(),
            message_list: self.message_list.get(),
            reader: self.reader.get(),
            status: self.status.get(),
            mail_view: self.mail_view.get(),
            panes: self.panes.get(),
        }
    }
}

/// The whole window.
pub fn window(w: &Widgets) -> Layout<Msg> {
    column().children((
        toolbar().fixed(Dip(TOOLBAR_H)),
        stack()
            .children((mail_view(w), account::page(&w.account)))
            .fill(1),
        row()
            .padding(Insets::new(Dip(8.0), Dip(3.0), Dip(8.0), Dip(0.0)))
            .child(label("Starting...").bind(&w.status).fill(1))
            .fixed(Dip(STATUS_H)),
    ))
}

fn toolbar() -> Layout<Msg> {
    let tool = |text: &str, icon: Lucide, msg: Msg| {
        button(text).icon(icon).on_click(msg).width(Dip(BUTTON_W))
    };
    row()
        .gap(8)
        .align(Align::Center)
        .padding(Insets::symmetric(Dip(8.0), Dip(5.0)))
        .children((
            tool("Get mail", Lucide::Inbox, Msg::GetMail),
            tool("New", Lucide::Pencil, Msg::Compose),
            tool("Reply", Lucide::Reply, Msg::Reply),
            tool("Accounts", Lucide::Users, Msg::Accounts),
            label("esMail for LazyOS").align_y(Align::Center).fill(1),
        ))
}

fn mail_view(w: &Widgets) -> impl LayoutExt<Msg> {
    panel(
        row().children((
            list()
                .column("Folders", Fill)
                .on_select(Msg::Folder)
                .bind(&w.folder_list)
                .width(Dip(FOLDERS_W)),
            stack()
                .children((
                    panel(
                        row().children((
                            list()
                                .column("From", Dip(96.0))
                                .column("Subject", Fill)
                                .column("Date", Dip(124.0))
                                .on_select(Msg::Message)
                                .bind(&w.message_list)
                                .width(Dip(LIST_W)),
                            build(Reader::new).bind(&w.reader).fill(1),
                        )),
                    )
                    .plain()
                    .bind(&w.panes),
                    compose::page(&w.compose),
                ))
                .fill(1),
        )),
    )
    .plain()
    .bind(&w.mail_view)
}
