//! The compose form: a new message, a reply or a forward, sent through the
//! account's SMTP server. It covers the message list and the reading pane
//! while it is open.

use std::rc::Rc;

use esmail::compose::ComposeState;
use xui_core::app::Ui;
use xui_core::arrange::{
    Align, Handle, LayoutExt, Track, button, column, edit, grid, label, multiline_edit, panel, row,
};
use xui_core::icon::Lucide;
use xui_core::widget::{Button, Edit, HasText, Label, MultilineEdit, Panel};

use super::Msg;

/// The handles [`page`] fills.
#[derive(Default)]
pub struct ComposeWidgets {
    panel: Handle<Panel<Msg>>,
    from: Handle<Label<Msg>>,
    to: Handle<Edit<Msg>>,
    cc: Handle<Edit<Msg>>,
    subject: Handle<Edit<Msg>>,
    body: Handle<MultilineEdit<Msg>>,
    send: Handle<Button<Msg>>,
}

/// The form: the sender, the header fields in a two-column grid, the body
/// taking the rest and the buttons at the bottom right.
pub fn page(w: &ComposeWidgets) -> impl LayoutExt<Msg> {
    let caption = |text: &str| label(text).align_y(Align::Center);
    panel(
        column().padding(8).gap(8).children((
            label("").bind(&w.from),
            grid([Track::Auto, Track::Fill(1)]).gap(8).children((
                caption("To"),
                edit().placeholder("name@example.com, ...").bind(&w.to),
                caption("Cc"),
                edit().bind(&w.cc),
                caption("Subject"),
                edit().bind(&w.subject),
            )),
            multiline_edit().bind(&w.body).fill(1),
            row().gap(8).justify(Align::End).children((
                button("Discard")
                    .icon(Lucide::Trash2)
                    .on_click(Msg::CloseCompose),
                button("Send")
                    .icon(Lucide::Send)
                    .primary()
                    .on_click(Msg::Send)
                    .bind(&w.send),
            )),
        )),
    )
    .plain()
    .bind(&w.panel)
}

pub struct ComposePage {
    panel: Rc<Panel<Msg>>,
    from: Rc<Label<Msg>>,
    to: Rc<Edit<Msg>>,
    cc: Rc<Edit<Msg>>,
    subject: Rc<Edit<Msg>>,
    body: Rc<MultilineEdit<Msg>>,
    send: Rc<Button<Msg>>,
    /// What the form was opened with: the account to send from and the
    /// threading headers a reply carries, which have no field.
    state: Option<(usize, ComposeState)>,
}

impl ComposePage {
    /// The form [`page`] mounted.
    pub fn new(w: &ComposeWidgets) -> ComposePage {
        ComposePage {
            panel: w.panel.get(),
            from: w.from.get(),
            to: w.to.get(),
            cc: w.cc.get(),
            subject: w.subject.get(),
            body: w.body.get(),
            send: w.send.get(),
            state: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.state.is_some()
    }

    /// Opens the form with `state`, sending from account `account`, shown as
    /// `from`.
    pub fn open(&mut self, ui: &Ui<Msg>, account: usize, from: &str, state: ComposeState) {
        self.from.set_text(&format!("From: {from}"));
        self.to.set_text(&state.to);
        self.cc.set_text(&state.cc);
        self.subject.set_text(&state.subject);
        self.body.set_text(&state.body);
        let focus_body = !state.to.is_empty();
        self.state = Some((account, state));
        self.set_sending(ui, false);
        self.set_shown(ui, true);
        if focus_body {
            self.body.focus()
        } else {
            self.to.focus()
        }
    }

    pub fn close(&mut self, ui: &Ui<Msg>) {
        self.state = None;
        self.set_shown(ui, false);
    }

    fn set_shown(&self, ui: &Ui<Msg>, shown: bool) {
        ui.set_visible(self.panel.id(), shown);
    }

    /// While a send is in flight the button is off, so one click sends once.
    pub fn set_sending(&self, ui: &Ui<Msg>, sending: bool) {
        ui.set_enabled(self.send.id(), !sending);
        self.send
            .set_text(if sending { "Sending..." } else { "Send" });
    }

    /// The account and the message as the form now holds it.
    pub fn message(&self) -> Option<(usize, ComposeState)> {
        let (account, opened) = self.state.as_ref()?;
        let mut state = opened.clone();
        state.to = self.to.text();
        state.cc = self.cc.text();
        state.subject = self.subject.text();
        state.body = self.body.text();
        Some((*account, state))
    }
}
