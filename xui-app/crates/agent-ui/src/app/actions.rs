//! What a click in the transcript does.

use super::*;

impl LazyAgentApp {
    pub(super) fn open_link(&mut self, ui: &Ui<Msg>, url: &str) {
        // The painter only makes web links clickable; the address is checked
        // again here because it is what a launcher is handed.
        if !crate::markdown::is_web(url) {
            return;
        }
        // Built when needed: its message names the address.
        let built = TaskDialog::new(
            ui,
            "Open this link?",
            &format!("{url}\n\nIt opens in your browser. Only continue if you trust the address."),
        )
        .and_then(|d| d.icon(TaskDialogIcon::Warning).command("Open"))
        .map(|d| d.on_action(|action| Some(Msg::LinkChoice(action))));
        if let Ok(dialog) = built {
            dialog.open();
            self.link_dialog = Some(dialog);
            self.pending_link = Some(url.to_string());
            self.say(Report::LinkAsked(url.to_string()));
        }
    }

    pub(super) fn clicked(&mut self, ui: &mut Ui<Msg>, action: ViewAction) {
        match action {
            ViewAction::Link(url) => self.open_link(ui, &url),
            ViewAction::Copy(text) => {
                ui.set_clipboard_text(&text);
                self.say(Report::Copied);
            }
            ViewAction::Moved => self.set_interval(ui, drain::BUSY_MS),
            ViewAction::Toggle(id) => {
                if !self.opened.remove(&id) {
                    self.opened.insert(id);
                }
                self.rows_dirty = true;
                self.set_interval(ui, drain::BUSY_MS);
            }
        }
    }
}
