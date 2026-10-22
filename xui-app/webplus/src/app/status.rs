//! What the browser's state shows outside the page: the trace line, the status bar, the toolbar
//! buttons and the downloads strip.

use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_core::icon::Lucide;
use xui_core::widget::{HasText, Placeable};

use super::{Browser, Msg};
use crate::chrome::Command;

impl Browser {
    /// Says where the controls are, for a session (`xui_app::probe`): the page as a whole and
    /// its top-left corner as a one-pixel control, so a script can click page coordinates.
    pub(super) fn announce(&self, ui: &Ui<Msg>) {
        let page = ui.bounds(self.page.id());
        self.announced.set((page.width(), page.height()));
        let mut named = vec![
            ("address", ui.bounds(self.address.id())),
            ("back", ui.bounds(self.back.id())),
            ("forward", ui.bounds(self.forward.id())),
            ("reload", ui.bounds(self.reload.id())),
            ("shield", ui.bounds(self.shield.id())),
            ("page", page),
            (
                "page_origin",
                Rect::new(page.left, page.top, page.left + 1, page.top + 1),
            ),
        ];
        // The bar's own controls, while it is up: a session answers it by name.
        if self.bar.shown().is_some() {
            named.extend(self.bar.controls(ui));
        }
        if self.hang.visible() {
            named.extend(self.hang.controls(ui));
        }
        for (name, rect) in named {
            (self.host.probe)(&self.caption, name, rect);
        }
    }

    pub(super) fn trace(&self, line: &str) {
        if self.host.traces {
            crate::marker(line);
        }
    }

    pub(super) fn set_loading(&mut self, on: bool) {
        self.loading.set(on);
        self.throbber.set_spinning(on);
    }

    /// Shows `text` in the status bar until something replaces it; a link under the pointer is
    /// shown over it.
    pub(super) fn set_status(&mut self, text: &str) {
        self.message = text.to_owned();
        self.refresh_status();
    }

    pub(super) fn refresh_status(&self) {
        let text = if self.hover.is_empty() {
            &self.message
        } else {
            &self.hover
        };
        if *self.shown.borrow() != *text {
            self.status.set_text(text);
            self.shown.replace(text.clone());
        }
    }

    /// The address the bar shows for the engine's `url`: our pages by name.
    pub(super) fn shown_url(&self, url: &str) -> String {
        self.internal.shown(url).to_owned()
    }

    /// Enables Back, Forward and Stop, and turns Reload into Stop while a page
    /// loads, touching them only when that changed.
    pub(super) fn update_buttons(&self) {
        let state = [
            self.state.can_back,
            self.state.can_forward,
            self.state.loading,
        ];
        if self.buttons.replace(Some(state)) == Some(state) {
            return;
        }
        self.back.set_enabled(state[0]);
        self.forward.set_enabled(state[1]);
        let (icon, tip) = if state[2] {
            (Lucide::X, "Stop (Esc)")
        } else {
            (Lucide::RefreshCw, "Reload (F5)")
        };
        self.reload.set_icon(Some(icon));
        let _ = self.reload.set_tooltip(tip);
        self.menu.set_enabled(Command::Back.id(), state[0]);
        self.menu.set_enabled(Command::Forward.id(), state[1]);
        self.menu.set_enabled(Command::Stop.id(), state[2]);
    }

    /// Shows the running downloads in the status bar, or hides that part.
    pub(super) fn update_downloads(&self, ui: &Ui<Msg>) {
        let summary = self.transfers.summary();
        ui.set_visible(self.download_label.id(), summary.is_some());
        let percent = summary.as_ref().and_then(|s| s.percent);
        ui.set_visible(self.download_bar.id(), percent.is_some());
        if let Some(summary) = summary {
            self.download_label.set_text(&summary.text);
            self.download_bar.set_value(percent.unwrap_or(0));
        }
    }
}
