//! Settings the user changes: theme, picture height, server, handle.

use super::*;

impl LazySkyApp {
    /// Writes `settings.toml` on the api thread.
    pub(super) fn save_settings(&mut self) {
        let seq = self.seq();
        let settings = self.settings.clone();
        self.ask(seq, Request::SaveSettings { seq, settings }, Route::Ignore);
    }

    pub(super) fn apply_theme(&self, ui: &Ui<Msg>) {
        let theme = match self.settings.theme {
            lazysky_core::ThemeChoice::System => self.system_theme,
            lazysky_core::ThemeChoice::Light => Theme::light(),
            lazysky_core::ThemeChoice::Dark => Theme::dark(),
        };
        ui.set_theme(theme);
    }

    /// Hands the picture height setting to both feeds and re-lays their
    /// rows out.
    pub(super) fn apply_max_image_height(&mut self) {
        let height = self.settings.max_image_height as i32;
        for pane in [HOME, DETAIL] {
            let mut s = self.view(pane).state.borrow_mut();
            if s.max_image_h != height {
                s.max_image_h = height;
                for i in 0..s.feed.len() {
                    if let Some(row) = s.feed.row_mut(i) {
                        row.dirty = true;
                    }
                }
                s.moved = true;
            }
        }
    }

    pub(super) fn settings_theme(&mut self, ui: &Ui<Msg>, index: usize) {
        self.settings.theme = match index {
            1 => lazysky_core::ThemeChoice::Light,
            2 => lazysky_core::ThemeChoice::Dark,
            _ => lazysky_core::ThemeChoice::System,
        };
        self.apply_theme(ui);
        self.save_settings();
    }

    pub(super) fn settings_max_height(&mut self, height: i32) {
        let range = lazysky_core::settings::MAX_IMAGE_HEIGHT;
        let height = u32::try_from(height).unwrap_or(0);
        self.settings.max_image_height = height.clamp(*range.start(), *range.end());
        self.apply_max_image_height();
        self.save_settings();
    }

    pub(super) fn settings_remember(&mut self, on: bool) {
        self.settings.remember_handle = on;
        self.settings.handle = if on {
            self.account.as_ref().map(|a| a.handle.clone())
        } else {
            None
        };
        self.save_settings();
    }

    pub(super) fn settings_service(&mut self) {
        let typed = self.settings_page.typed_service();
        let mut next = self.settings.clone();
        next.service = typed;
        next.normalise();
        self.settings.service = next.service.clone();
        let seq = self.seq();
        let service = self.settings.service.clone();
        self.ask(seq, Request::Configure { seq, service }, Route::Ignore);
        self.save_settings();
        self.settings_page
            .say(&format!("Signing in will use {}.", self.settings.service));
    }
}
