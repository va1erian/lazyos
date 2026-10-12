//! What the settings, the zoom, the desktop's theme and the window's focus
//! decide about how the Terminal looks: the font and its cell, the colours,
//! the cursor, and so the grid's rows (the engine and the pty follow).

use lazyterm::palette::scheme_by_name;
use lazyterm::settings::{MAX_GRID_COLS, MAX_GRID_ROWS, MIN_COLS, MIN_ROWS};
use xui_app::sys::monotonic_ns;
use xui_core::app::Ui;
use xui_core::Rect;

use super::conf::Conf;
use super::fonts;
use super::look;
use super::badge::BADGE_MILLIS;
use super::metrics::Metrics;
use super::view::{max_offset, Shown};
use super::{Msg, Terminal};

/// How long the window must rest before its grid is remembered.
const SAVE_DELAY_MS: u64 = 2000;
/// Zoom steps either way.
const MAX_ZOOM: i32 = 30;

/// Milliseconds on the monotonic clock.
pub fn now_ms() -> u64 {
    monotonic_ns() / 1_000_000
}

/// The grid a window holds: whole cells inside the padding, at least 20 x 5
/// and at most the bounds a setting may ask for.
pub fn grid_for(rect: Rect, m: &Metrics) -> (usize, usize) {
    let cols = ((rect.width() - 2 * m.pad) / m.cell_w.max(1)).max(0) as usize;
    let rows = ((rect.height() - 2 * m.pad) / m.line_h.max(1)).max(0) as usize;
    (cols.clamp(MIN_COLS, MAX_GRID_COLS), rows.clamp(MIN_ROWS, MAX_GRID_ROWS))
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

    /// The grid's size from the window, the cell and the font: columns and
    /// rows follow the visible width and height (at least 20 x 5). A change
    /// reflows the engine, tells the program (`TIOCSWINSZ`, so `SIGWINCH`),
    /// and is reported as `TERM:RESIZE`; `TERM:FONT` reports the font and grid.
    pub(super) fn relayout(&mut self, ui: &mut Ui<Msg>) {
        let (family, size, metrics) = {
            let look = self.look.borrow();
            (look.family.clone(), look.metrics.size, look.metrics)
        };
        // The window is about to take the size that was asked for: the grid
        // it has now is not what the user wants.
        if !self.settling {
            let (cols, rows) = grid_for(ui.client_rect(), &metrics);
            let have = {
                let term = self.term.borrow();
                (term.cols(), term.rows())
            };
            if have != (cols, rows) {
                self.apply_grid(ui, cols, rows);
            }
        }
        self.size_hints(&metrics);
        let (cols, rows) = {
            let term = self.term.borrow();
            (term.cols(), term.rows())
        };
        let marker = format!("{family}:{}:{cols}x{rows}", size as u32);
        if self.marker.as_deref() != Some(marker.as_str()) {
            println!("TERM:FONT:{marker}");
            self.marker = Some(marker);
        }
        if !self.sized {
            self.sized = true;
            println!("TERM:RESIZE:{cols}x{rows}");
        }
    }

    /// Resize the engine (reflowing it), keep the view on its text, then tell
    /// the program.
    fn apply_grid(&mut self, ui: &mut Ui<Msg>, cols: usize, rows: usize) {
        {
            let mut term = self.term.borrow_mut();
            let view_top = term.screen_top().saturating_sub(self.offset.get() as u64);
            let remap = term.resize(cols, rows);
            let offset = if self.offset.get() == 0 {
                0
            } else {
                term.screen_top().saturating_sub(remap.line(view_top)) as usize
            };
            self.offset.set(offset.min(max_offset(&term, rows)));
        }
        self.master.set_size(rows as u16, cols as u16);
        // The cursor is solid while the size changes.
        self.blink.activity(now_ms());
        self.sync_cursor_phase();
        println!("TERM:RESIZE:{cols}x{rows}");
        if self.sized {
            self.show_badge(ui, cols, rows);
            self.shown = Shown::default();
            // Only the user's own resizing is remembered, not what a font or
            // zoom change does to the grid.
            if self.remember && self.zoom == 0 {
                self.save = Some((cols, rows, now_ms() + SAVE_DELAY_MS));
            }
        }
        self.sized = true;
    }

    /// The window's size changed: the root node follows it, then the grid.
    pub(super) fn window_resized(&mut self, ui: &mut Ui<Msg>) {
        self.settling = false;
        let rect = ui.client_rect();
        self.root.set_bounds(rect);
        self.remember = true;
        self.relayout(ui);
        self.remember = false;
        self.refresh(ui);
    }

    /// Ask the compositor for a window that holds `grid`; the grid is applied
    /// when its answer arrives (`window_resized`).
    pub(super) fn request_grid(&self, grid: (usize, usize)) {
        let m = self.look.borrow().metrics;
        let scale = m.scale.max(1);
        let width = (grid.0 as i32 * m.cell_w + 2 * m.pad) / scale;
        let height = (grid.1 as i32 * m.line_h + 2 * m.pad) / scale;
        self.backend.request_size(width.max(1) as u32, height.max(1) as u32);
    }

    /// The smallest window the compositor lets the user make: 20 x 5 cells and
    /// the padding, in design pixels.
    fn size_hints(&self, m: &Metrics) {
        let scale = m.scale.max(1);
        let min_w = (MIN_COLS as i32 * m.cell_w + 2 * m.pad + scale - 1) / scale;
        let min_h = (MIN_ROWS as i32 * m.line_h + 2 * m.pad + scale - 1) / scale;
        self.backend.set_size_hints(min_w as u32, min_h as u32, 0, 0);
    }

    /// Show `100 x 30` in the middle of the window for a moment.
    fn show_badge(&mut self, ui: &mut Ui<Msg>, cols: usize, rows: usize) {
        *self.badge.borrow_mut() = Some(format!("{cols} x {rows}"));
        if let Some(id) = self.badge_timer.take() {
            ui.kill_timer(id);
        }
        self.badge_timer.set(Some(ui.set_timer(BADGE_MILLIS)));
    }

    /// The badge's time is up: its timer stops with it.
    pub(super) fn hide_badge(&mut self, ui: &mut Ui<Msg>) {
        if let Some(id) = self.badge_timer.take() {
            ui.kill_timer(id);
        }
        if self.badge.borrow_mut().take().is_some() {
            if let Some(layout) = self.layout.get() {
                let rect = Rect::new(0, 0, layout.width, layout.height);
                ui.invalidate_rect(self.root.id(), rect);
            }
        }
    }

    /// Remember the grid once the window has rested (or now, on closing).
    pub(super) fn save_size(&mut self, now: bool) {
        if let Some((cols, rows, due)) = self.save {
            if now || now_ms() >= due {
                self.save = None;
                self.follower.save_last_size(cols, rows);
            }
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
        println!("TERM:FOCUS:{}", u8::from(focused));
        // A program that asked (`?1004`) hears about it.
        let report = self.term.borrow().focus_report(focused);
        if let Some(report) = report {
            self.send(report);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::new(15.0, 1, 130, 288, 19)
    }

    #[test]
    fn the_grid_is_whole_cells_inside_the_padding() {
        let m = metrics();
        assert_eq!(grid_for(Rect::new(0, 0, 640, 400), &m), (68, 19));
        assert_eq!(grid_for(Rect::new(0, 0, 20 + 9 * 100, 20 + 20 * 30), &m), (100, 30));
        // One pixel short of another column or row.
        assert_eq!(grid_for(Rect::new(0, 0, 20 + 9 * 100 - 1, 20 + 20 * 30 - 1), &m), (99, 29));
    }

    #[test]
    fn a_window_smaller_than_the_floor_still_has_20_by_5() {
        let m = metrics();
        assert_eq!(grid_for(Rect::new(0, 0, 50, 40), &m), (MIN_COLS, MIN_ROWS));
        assert_eq!(grid_for(Rect::new(0, 0, 0, 0), &m), (MIN_COLS, MIN_ROWS));
        assert_eq!(grid_for(Rect::new(0, 0, -5, -5), &m), (MIN_COLS, MIN_ROWS));
    }

    #[test]
    fn a_huge_window_stops_at_the_bounds() {
        let m = metrics();
        assert_eq!(grid_for(Rect::new(0, 0, 100_000, 100_000), &m), (MAX_GRID_COLS, MAX_GRID_ROWS));
    }

    #[test]
    fn the_scale_multiplies_the_cell_not_the_count() {
        let one = metrics();
        let two = Metrics::new(15.0, 2, 130, 576, 38);
        assert_eq!(grid_for(Rect::new(0, 0, 640, 400), &one), (68, 19));
        assert_eq!(grid_for(Rect::new(0, 0, 1280, 800), &two), (68, 19));
    }
}
