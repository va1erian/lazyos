//! The page's JavaScript dialogs (`alert`, `confirm`, `prompt`), drawn as xui dialogs over the
//! window.
//!
//! WebKit's UI client hands the UI process a listener and waits for it to be called; the UI
//! process passes the question to the window (`Event::Dialog`) and returns at once, so the frame
//! mailbox never blocks behind a person reading a message. The page's process waits for the
//! answer (that is what a dialog is); nothing else does. One dialog is open at a time because the
//! page cannot ask a second before the first is answered.
//!
//! The text of a prompt's default value is not shown in the field: xui's prompt dialog takes its
//! initial text when it is built, and these are built once.

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Dialog, DialogAction};
use xui_webkit::engine::DialogKind;

use crate::app::Msg;

/// The dialog that is open and the page's question it answers.
struct Open {
    id: u64,
    kind: DialogKind,
}

/// An answer to give the engine.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub id: u64,
    pub accepted: bool,
    pub value: String,
}

pub struct Dialogs {
    alert: Dialog<Msg>,
    confirm: Dialog<Msg>,
    prompt: Dialog<Msg>,
    open: Option<Open>,
}

impl Dialogs {
    pub fn new(ui: &Ui<Msg>) -> Result<Dialogs> {
        let answer = |action| Some(Msg::Dialog(action));
        Ok(Dialogs {
            alert: Dialog::message(ui, "", "")?.on_action(answer),
            confirm: Dialog::confirm(ui, "", "")?.on_action(answer),
            prompt: Dialog::prompt(ui, "", "", "")?.on_action(answer),
            open: None,
        })
    }

    /// Opens the dialog for question `id`. A question that arrives while another is open
    /// replaces it: the page asked it, so the earlier one was answered or its page is gone (the
    /// earlier one is returned, to be answered "no").
    pub fn show(&mut self, id: u64, kind: DialogKind, title: &str, text: &str) -> Option<Answer> {
        let replaced = self.dismiss();
        let dialog = self.widget(kind);
        dialog.set_title(title);
        dialog.set_message(text);
        dialog.open();
        self.open = Some(Open { id, kind });
        replaced
    }

    /// The person answered: what to tell the engine. `None` when no question is open.
    pub fn answer(&mut self, action: DialogAction) -> Option<Answer> {
        let open = self.open.take()?;
        Some(match action {
            DialogAction::Accept(value) => Answer {
                id: open.id,
                accepted: true,
                // Only a prompt has text.
                value: if open.kind == DialogKind::Prompt {
                    value
                } else {
                    String::new()
                },
            },
            DialogAction::Cancel => Answer {
                id: open.id,
                accepted: false,
                value: String::new(),
            },
        })
    }

    /// Closes the open dialog without an answer from the person (the page went away); the
    /// engine is told "no".
    pub fn dismiss(&mut self) -> Option<Answer> {
        let open = self.open.take()?;
        self.widget(open.kind).close();
        Some(Answer {
            id: open.id,
            accepted: false,
            value: String::new(),
        })
    }

    /// The number of the question that is open.
    pub fn open_id(&self) -> Option<u64> {
        self.open.as_ref().map(|open| open.id)
    }

    /// Whether a question is open.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn widget(&self, kind: DialogKind) -> &Dialog<Msg> {
        match kind {
            DialogKind::Alert => &self.alert,
            DialogKind::Confirm => &self.confirm,
            DialogKind::Prompt => &self.prompt,
        }
    }
}

/// What the dialog's title says: whose question it is (`<host> says`), or the page's address
/// when it has no host (a file, a page of ours).
pub fn title_for(url: &str) -> String {
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
        .map(|authority| authority.rsplit('@').next().unwrap_or(authority))
        .filter(|host| !host.is_empty());
    match host {
        Some(host) => format!("{host} says"),
        None => String::from("This page says"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_the_host_that_asks() {
        assert_eq!(
            title_for("https://theoldnet.com/webkit/x.html"),
            "theoldnet.com says"
        );
        assert_eq!(title_for("http://u:p@host:8080/a?b"), "host:8080 says");
        assert_eq!(title_for("file:///tmp/x.html"), "This page says");
        assert_eq!(title_for("data:text/html,x"), "This page says");
        assert_eq!(title_for(""), "This page says");
    }
}
