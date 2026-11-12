//! One error dialog: a title, a sentence that says what failed and why.
//!
//! An app builds one at start-up and shows it with whatever failed. The error
//! is anything that displays: an `io::Error`, a `String`, a `CallError` (whose
//! `Display` is the service's own sentence plus its hint, or "the call
//! failed: <errno>" for a transport failure), so no app formats a call
//! failure by hand.

use std::fmt::Display;

use xui_core::app::Ui;
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::Dialog;

/// A message dialog an app shows its failures in.
pub struct ErrorDialog<M: 'static>(Dialog<M>);

impl<M: 'static> ErrorDialog<M> {
    /// Builds the (hidden) dialog titled `title`. `on_close` maps its
    /// dismissal to the app's message, if it wants one.
    pub fn new(
        ui: &Ui<M>,
        title: &str,
        on_close: impl Fn() -> Option<M> + 'static,
    ) -> Result<ErrorDialog<M>> {
        let dialog = Dialog::message(ui, title, "")?.on_action(move |_| on_close());
        Ok(ErrorDialog(dialog))
    }

    /// Shows `error` as the dialog's text.
    pub fn show(&self, error: &dyn Display) {
        self.0.set_message(&error.to_string());
        self.0.open();
    }

    /// Shows `what` (what the app was doing) and then `error`: "Could not
    /// open /a.pdf: no such file".
    pub fn show_while(&self, what: &str, error: &dyn Display) {
        self.show(&format_args!("{what}: {error}"));
    }

    /// Shows `error` under another title than the dialog was built with.
    pub fn show_titled(&self, title: &str, error: &dyn Display) {
        self.0.set_title(title);
        self.show(error);
    }

    /// Whether the dialog is up (a modal gate for the app's hotkeys).
    pub fn is_open(&self) -> bool {
        self.0.is_open()
    }

    /// Takes the dialog down.
    pub fn close(&self) {
        self.0.close();
    }

    /// The dialog's widget, for a test that clicks it.
    pub fn id(&self) -> WidgetId {
        self.0.id()
    }

    /// The text now shown.
    pub fn text(&self) -> String {
        self.0.text()
    }
}
