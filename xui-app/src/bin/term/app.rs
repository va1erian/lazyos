//! The Terminal as an xui app: its state, and how each message changes it.

use std::cell::{Cell, RefCell};
use std::process::Child;
use std::rc::Rc;

use lazyterm::input::Button;
use lazyterm::Term as Engine;
use xui_app::backend::LazyOSBackend;
use xui_core::app::{App, Ui};
use xui_core::widget::FileDialog;
use xui_core::{Control, Key};

use super::bell::Limiter;
use super::blink::Blink;
use super::chrome::Chrome;
use super::clicks::Clicks;
use super::conf::{Conf, Follower};
use super::evidence::Evidence;
use super::look::Look;
use super::msg::Msg;
use super::perf::{PaintClock, Perf};
use super::timers::Timers;
use super::view::{Layout, Shown};
use super::{fonts, pty};

/// The app: the child shell, the pty master, and the engine it paints from.
pub(super) struct Terminal {
    pub(super) child: Child,
    pub(super) master: pty::Master,
    /// The shell side is gone (the master read `EIO` or end-of-file).
    pub(super) hung_up: bool,
    pub(super) term: Rc<RefCell<Engine>>,
    pub(super) typed: String,
    /// Exactly one `TERM:OUT` per command (`term/evidence.rs`).
    pub(super) evidence: Evidence,
    pub(super) root: Control<Msg>,
    /// What is on screen, so a drain repaints only the rows that changed;
    /// the painter's latest layout feeds it.
    pub(super) shown: Shown,
    pub(super) layout: Rc<Cell<Option<Layout>>>,
    /// `TERM:PERF` evidence (`term/perf.rs`).
    pub(super) perf: Perf,
    pub(super) paint_clock: Rc<PaintClock>,
    /// Lines the view is scrolled back from the bottom, shared with the
    /// painter.
    pub(super) offset: Rc<Cell<usize>>,
    /// A selection drag is in progress, the pointer's last position, the
    /// button a program sees held, and the triple-click memory.
    pub(super) drag: bool,
    pub(super) last_mouse: (i32, i32),
    pub(super) held: Option<Button>,
    pub(super) clicks: Clicks,
    /// Bytes for the child the pty would not take yet (a big paste).
    pub(super) out: Vec<u8>,
    /// The paste awaiting confirmation (its prompt is in `chrome`).
    pub(super) pending_text: Option<String>,
    /// How the grid looks (shared with the painter), the settings in force,
    /// the families found, and what follows `confd`.
    pub(super) look: Rc<RefCell<Look>>,
    pub(super) conf: Conf,
    pub(super) fonts: Rc<fonts::Fonts>,
    pub(super) follower: Follower,
    /// This window's zoom steps (never stored), the theme's darkness and the
    /// scheme last given to the engine, and the last `TERM:FONT` line.
    pub(super) zoom: i32,
    pub(super) dark: bool,
    pub(super) applied_scheme: Option<(String, bool)>,
    pub(super) marker: Option<String>,
    /// The cursor blink.
    pub(super) blink: Blink,
    /// Every timer (`timers.rs`), and the layers over the grid shared with the
    /// painter (`chrome.rs`).
    pub(super) timers: Timers,
    pub(super) chrome: Rc<RefCell<Chrome>>,
    /// The backend (size hints, size requests).
    pub(super) backend: Rc<LazyOSBackend>,
    /// A grid has been applied before (the first is not announced), and the
    /// window is still being sized to the `window/size` that was asked for.
    pub(super) sized: bool,
    pub(super) settling: bool,
    /// The size change being applied is the window's, so it is remembered.
    pub(super) remember: bool,
    /// The grid to remember in `confd` once the window has rested, and when.
    pub(super) save: Option<(usize, usize, u64)>,
    /// The tab character that follows a Shift+Tab key press is not sent.
    pub(super) skip_char: Option<char>,
    /// What the program told the window (OSC 0/2 and 7), as last shown, and the
    /// synchronized update being held back (`output.rs`).
    pub(super) title_generation: u64,
    pub(super) title: String,
    pub(super) cwd: Option<lazyterm::Cwd>,
    pub(super) sync_hold: Option<u64>,
    /// The bell's rate limit, the Save output dialog, and what the find bar's
    /// last finished scan counted (so `TERM:FIND` is printed once per result).
    pub(super) bell: Limiter,
    pub(super) save_dialog: FileDialog<Msg>,
    pub(super) found_reported: Option<usize>,
    /// The link under the pointer while Ctrl is held.
    pub(super) hover: Option<lazyterm::LinkHit>,
}

impl Terminal {
    pub(super) fn finish(&mut self, ui: &mut Ui<Msg>) {
        self.save_size(true);
        println!("TERM:EXIT:PASS");
        ui.quit();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Reached on every way out, including the compositor dropping the
        // window (`SetCloseMode`'s grace) with no answer from the app.
        println!("TERM:QUIT:PASS");
    }
}

impl App for Terminal {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                let bytes = self.drain();
                if !bytes.is_empty() {
                    self.output(ui, &bytes);
                }
                if let Some(conf) = self.follower.poll() {
                    self.apply_conf(ui, conf);
                }
                // A window that never answered the size request is left as it is.
                if std::mem::take(&mut self.settling) {
                    self.relayout(ui);
                    self.refresh(ui);
                }
                self.save_size(false);
                self.follow_theme(ui);
                if self.hung_up {
                    self.finish(ui);
                }
            }
            Msg::Autoscroll => self.autoscroll(ui),
            Msg::Flush => self.flush_out(),
            Msg::Blink => self.blink_tick(ui),
            Msg::Focus(focused) => self.focus_changed(ui, focused),
            Msg::Resized => self.window_resized(ui),
            Msg::BadgeOff => self.hide_badge(ui),
            Msg::SyncExpired => self.sync_expired(ui),
            Msg::FlashOff => self.flash_off(ui),
            Msg::FindScan => self.find_scan(ui),
            Msg::SaveChosen(path) => self.save_output(ui, path),
            Msg::SaveCancelled => self.root.focus(),
            // A layer that takes the keyboard (a prompt, the menu, the find
            // bar, the preferences) answers first.
            Msg::Char(ch) if self.modal() => {
                self.skip_char = None;
                self.modal_char(ui, ch);
            }
            Msg::Key(key, mods) if self.modal() => self.modal_key(ui, key, mods),
            Msg::Char(ch) => {
                self.touch(ui);
                if !skipped(self.skip_char.take(), ch) {
                    self.key(ch, ui);
                }
            }
            // Both input paths also deliver a plain Enter or Backspace as a
            // `Char`: handling the key too would send each one twice. With Alt
            // or Ctrl there is no `Char`, and the chord is encoded.
            Msg::Key(Key::RETURN | Key::BACK, mods) if !(mods.alt || mods.ctrl) => {}
            Msg::Key(key, mods) => {
                self.touch(ui);
                self.key_chord(ui, key, mods);
            }
            Msg::Wheel(delta, mods, x, y) => {
                self.touch(ui);
                self.wheel(ui, delta, mods, (x, y));
            }
            Msg::Mouse(mouse) => {
                self.touch(ui);
                self.mouse(ui, mouse);
            }
            Msg::Dropped(paths) => self.dropped(ui, paths),
            Msg::Close => self.request_close(ui),
        }
        self.pump(ui);
    }
}

/// Whether `ch` is the `Char` that follows a key a layer already handled.
fn skipped(pending: Option<char>, ch: char) -> bool {
    pending == Some(ch) || (pending == Some('\r') && ch == '\n')
}

#[cfg(test)]
mod tests {
    use super::skipped;

    #[test]
    fn the_char_after_a_handled_enter_is_skipped_once() {
        assert!(skipped(Some('\r'), '\r'));
        assert!(skipped(Some('\r'), '\n'));
        assert!(!skipped(Some('\r'), 'a'));
        assert!(!skipped(None, '\r'));
        assert!(skipped(Some('\x1b'), '\x1b'));
    }
}
