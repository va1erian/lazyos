#![forbid(unsafe_code)]

//! Platform seams for the editor. Today that is only the clipboard.
//!
//! The editor copies and pastes through the [`Clipboard`] trait. By default it
//! is backed by the window's portable clipboard
//! ([`Backend::clipboard_text`](xui_core::backend::Backend::clipboard_text) via
//! [`Ui`]): the real OS clipboard on the Win32 and canvas backends, an
//! in-process store on backends without one. An app that wants something else
//! implements [`Clipboard`] and passes it to
//! [`Editor::with_clipboard`](crate::Editor::with_clipboard).

use std::cell::RefCell;

use xui_core::app::Ui;

/// Text-only clipboard access.
pub trait Clipboard {
    /// The clipboard's current text, or `None` when it holds none or is
    /// unavailable.
    fn text(&self) -> Option<String>;

    /// Replaces the clipboard's text.
    fn set_text(&self, text: &str);
}

thread_local! {
    /// The in-process clipboard, shared by every editor on this thread.
    static IN_PROCESS: RefCell<String> = const { RefCell::new(String::new()) };
}

/// The in-process clipboard: every editor on the thread shares one buffer.
#[derive(Clone, Copy, Debug, Default)]
pub struct InProcessClipboard;

impl Clipboard for InProcessClipboard {
    fn text(&self) -> Option<String> {
        IN_PROCESS.with(|value| {
            let value = value.borrow();
            (!value.is_empty()).then(|| value.clone())
        })
    }

    fn set_text(&self, text: &str) {
        IN_PROCESS.with(|value| *value.borrow_mut() = text.to_string());
    }
}

/// The clipboard of the window `ui` belongs to.
struct BackendClipboard<M: 'static>(Ui<M>);

impl<M: 'static> Clipboard for BackendClipboard<M> {
    fn text(&self) -> Option<String> {
        self.0.clipboard_text()
    }

    fn set_text(&self, text: &str) {
        self.0.set_clipboard_text(text);
    }
}

/// The clipboard the editor uses by default: `ui`'s portable clipboard.
pub fn clipboard<M: 'static>(ui: &Ui<M>) -> Box<dyn Clipboard> {
    Box::new(BackendClipboard(ui.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_in_process_clipboard_round_trips() {
        let clipboard = InProcessClipboard;
        clipboard.set_text("hello");
        assert_eq!(clipboard.text().as_deref(), Some("hello"));
    }

    #[test]
    fn two_in_process_handles_share_one_buffer() {
        let first = InProcessClipboard;
        let second = InProcessClipboard;
        first.set_text("shared");
        assert_eq!(second.text().as_deref(), Some("shared"));
    }
}
