//! What the settings, the zoom, the desktop's theme and the window's focus
//! decide about how the Terminal looks: the font and its cell, the colours,
//! the cursor, and so the grid's rows (the engine and the pty follow).

use lazyterm::palette::scheme_by_name;
use xui_app::sys::monotonic_ns;
use xui_core::app::Ui;

use super::conf::Conf;
use super::fonts;
use super::look;
use super::{Msg, Terminal, COLS};

/// The most rows the grid grows to (a tiny font in a tall window).
const MAX_ROWS: usize = 200;
/// Zoom steps either way.
const MAX_ZOOM: i32 = 30;

/// Milliseconds on the monotonic clock.
pub fn now_ms() -> u64 {
    monotonic_ns() / 1_000_000
}

impl Terminal {
    /// New settings arrived from `confd`.
    pub(super) fn apply_conf(&mut self, ui: &mut Ui<Msg>, conf: Conf) {
        self.conf = conf;
        self.restyle(ui);
    }

    /// Ctrl+= / Ctrl+- / Ctrl+wheel (`delta` steps) or Ctrl+0 (`None`): this
    /// window's zoom, never stored.
    pub(super) fn zoom_by(&mut self, ui: &mut Ui<Msg>, delta: Option<i32>) {
        let zoom = match delta {
            Some(delta) => (self.zoom + delta).clamp(-MAX_ZOOM, MAX_ZOOM),
            None => 0,
        };
        if zoom != self.zoom {
            self.zoom = zoom;
            self.restyle(ui);
        }
    }

    /// Rebuild everything the settings and zoom decide: the font and its
    /// metrics, the scheme, the scrollback, the blink, the grid.
    pub(super) fn restyle(&mut self, ui: &mut Ui<Msg>) {
        let settings = self.conf.settings.clone();
        let family = self
            .fonts
            .catalog
            .choose(&settings.family, fonts::BUILT_IN)
            .to_string();
        let size = settings.zoomed_size(self.zoom) as f32;
        let metrics = look::measure(ui, &family, size, settings.line_spacing);
        let styles = self.fonts.catalog.styles(&family).unwrap_or_default();
        let dark = ui.theme_handle().get().is_dark;
        let mut changed = false;
        {
            let mut look = self.look.borrow_mut();
            changed |= look.metrics != metrics
                || look.family != family
                || look.styles != styles
                || look.bold_is_bright != settings.bold_is_bright;
            look.metrics = metrics;
            look.family = family;
            look.styles = styles;
            look.bold_is_bright = settings.bold_is_bright;
            look.cursor_shape = settings.cursor_shape;
            if changed {
                look.generation += 1;
            }
        }
        {
            let mut term = self.term.borrow_mut();
            let scheme = (settings.scheme.clone(), dark);
            if self.applied_scheme.as_ref() != Some(&scheme) {
                term.set_scheme(scheme_by_name(&settings.scheme, dark));
                self.applied_scheme = Some(scheme);
                changed = true;
            }
            term.set_scrollback(settings.scrollback);
            term.set_word_chars(&settings.word_chars);
        }
        self.dark = dark;
        self.blink
            .configure(settings.cursor_blink, self.conf.motion_none, now_ms());
        self.sync_cursor_phase();
        self.relayout(ui);
        if changed {
            self.look.borrow_mut().generation += 1;
        }
        self.refresh(ui);
    }

    /// The grid's size from the window and the line height: the columns stay
    /// the pty's 80 (a narrower tty wraps long commands, and the window clips
    /// what it cannot show; reflow is T4), the rows are what the window
    /// holds. The engine and the pty follow, and `TERM:FONT` reports it.
    pub(super) fn relayout(&mut self, ui: &mut Ui<Msg>) {
        let (family, size, metrics) = {
            let look = self.look.borrow();
            (look.family.clone(), look.metrics.size, look.metrics)
        };
        let height = ui.client_rect().height();
        let rows = (((height - 2 * metrics.pad) / metrics.line_h).max(1) as usize).min(MAX_ROWS);
        let now = {
            let mut term = self.term.borrow_mut();
            let before = (term.cols(), term.rows());
            if before != (COLS, rows) {
                term.resize(COLS, rows);
                // The program learns the new size (SIGWINCH) only when it
                // changed.
                self.master.set_size(rows as u16, COLS as u16);
                self.offset.set(0);
            }
            (term.cols(), term.rows())
        };
        let marker = format!("{family}:{}:{}x{}", size as u32, now.0, now.1);
        if self.marker.as_deref() != Some(marker.as_str()) {
            println!("TERM:FONT:{marker}");
            self.marker = Some(marker);
        }
    }

    /// The desktop went dark or light: the `desktop` scheme follows.
    pub(super) fn follow_theme(&mut self, ui: &mut Ui<Msg>) {
        let dark = ui.theme_handle().get().is_dark;
        if dark != self.dark {
            self.restyle(ui);
        }
    }

    /// The cursor is drawn at the blink's phase.
    pub(super) fn sync_cursor_phase(&mut self) {
        let mut look = self.look.borrow_mut();
        look.cursor_on = self.blink.cursor_on();
        look.focused = self.blink.focused();
    }

    /// Someone is here (a key, a press, output): the cursor shows and the
    /// blink starts over.
    pub(super) fn touch(&mut self, ui: &mut Ui<Msg>) {
        self.blink.activity(now_ms());
        if !self.look.borrow().cursor_on {
            self.sync_cursor_phase();
            self.refresh(ui);
        }
    }

    pub(super) fn focus_changed(&mut self, ui: &mut Ui<Msg>, focused: bool) {
        self.blink.set_focused(focused, now_ms());
        self.sync_cursor_phase();
        self.refresh(ui);
    }

    /// The blink timer fired.
    pub(super) fn blink_tick(&mut self, ui: &mut Ui<Msg>) {
        if self.blink.tick(now_ms()) {
            self.sync_cursor_phase();
            self.refresh(ui);
        }
    }
}
