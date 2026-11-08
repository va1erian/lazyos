//! The conveniences' actions: clear and reset, Save output, New Window, closing
//! over a running program, opening a link, and the bell.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lazyterm::settings::Bell;
use xui_app::platform::launcher;
use xui_core::app::Ui;

use super::appearance::now_ms;
use super::bell::{self, FLASH_MS};
use super::helpers::{sanitize_url, savable, suggested_name};
use super::jobs;
use super::msg::Msg;
use super::prompt::Prompt;
use super::timers::Timers;
use super::Terminal;

impl Terminal {
    /// Menu: forget the scrollback (the screen stays).
    pub(super) fn clear_scrollback(&mut self) {
        self.term.borrow_mut().clear_scrollback();
        self.offset.set(0);
    }

    /// Menu: reset the terminal (RIS: the screen, modes and colours; the
    /// program is not told).
    pub(super) fn reset_terminal(&mut self) {
        self.term.borrow_mut().feed(b"\x1bc");
        self.offset.set(0);
    }

    /// Menu: choose a file to save the output to.
    pub(super) fn open_save(&mut self) {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.save_dialog.set_suggested_name(&suggested_name(secs));
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute()) {
            self.save_dialog.set_initial_dir(home);
        }
        self.save_dialog.open();
    }

    /// The Save dialog chose `path`: write the screen and history there as
    /// plain text (`TERM:SAVE:<bytes>`).
    pub(super) fn save_output(&mut self, ui: &mut Ui<Msg>, path: PathBuf) {
        self.root.focus();
        if !savable(&path) {
            println!("TERM:SAVE:FAIL:path");
            return;
        }
        let text = self.term.borrow().dump_text();
        match std::fs::write(&path, text.as_bytes()) {
            Ok(()) => println!("TERM:SAVE:{}", text.len()),
            Err(error) => println!("TERM:SAVE:FAIL:{}", error.kind()),
        }
        self.redraw(ui);
    }

    /// Open another Terminal (as the start menu does: `init`'s `Launch`) in the
    /// directory the shell last reported, if it still exists.
    pub(super) fn new_window(&mut self) {
        let cwd = self.cwd_path().filter(|p| Path::new(p).is_absolute() && Path::new(p).is_dir()).unwrap_or_default();
        println!("TERM:NEWWIN:{cwd}");
        if let Err(code) = xui_app::shell::launch("terminal", &cwd) {
            println!("TERM:NEWWIN:FAIL:{}", -code);
        }
    }

    /// The window's close (the button or the menu): ask first when a program
    /// other than the shell runs in front.
    pub(super) fn request_close(&mut self, ui: &mut Ui<Msg>) {
        let shell = self.child.id() as i32;
        let foreground = jobs::foreground_group(self.master.fd());
        let state = jobs::shell_state(shell as u64);
        println!("TERM:CLOSE:CHECK:fg={foreground:?} shell={shell} state={state:?}");
        let busy = !self.hung_up && jobs::busy(foreground, shell, state);
        if !busy {
            self.finish(ui);
            return;
        }
        if self.chrome.borrow().prompt.is_none() {
            println!("TERM:CLOSE:CONFIRM");
            self.show_prompt(ui, Prompt::close());
        }
    }

    /// Put a confirmation bar up, with its probe rectangles.
    pub(super) fn show_prompt(&mut self, ui: &mut Ui<Msg>, prompt: Prompt) {
        if let Some(layout) = self.layout.get() {
            let rects = super::geometry::confirm_rects(&layout);
            for (name, r) in prompt.probe_names().into_iter().zip([rects.bar, rects.paste, rects.cancel]) {
                self.probe_rect(name, r);
            }
        }
        self.chrome.borrow_mut().prompt = Some(prompt);
        self.redraw(ui);
    }

    /// Show the target of `url` and ask before opening it.
    pub(super) fn ask_open(&mut self, ui: &mut Ui<Msg>, url: &str) {
        match sanitize_url(url) {
            Some(url) => {
                println!("TERM:LINK:CONFIRM:{url}");
                self.show_prompt(ui, Prompt::link(&url));
            }
            None => println!("TERM:LINK:REFUSED"),
        }
    }

    /// Open `url` in the app registered for its scheme, through `mimed`.
    pub(super) fn open_url(&mut self, url: &str) {
        let Some(url) = sanitize_url(url) else {
            println!("TERM:LINK:REFUSED");
            return;
        };
        println!("TERM:LINK:OPEN:{url}");
        if let Err(error) = launcher::open_url(&url) {
            println!("TERM:LINK:FAIL:{:?}", error.kind());
        }
    }

    /// BELs arrived: what the setting says, at most one a half second.
    pub(super) fn ring(&mut self, ui: &mut Ui<Msg>, rang: u32) {
        let Some(kind) = bell::decide(self.conf.settings.bell, rang, &mut self.bell, now_ms()) else {
            return;
        };
        match kind {
            Bell::Visual => {
                println!("TERM:BELL:visual");
                self.chrome.borrow_mut().flash = true;
                Timers::stop(&self.timers.flash, ui);
                Timers::start(&self.timers.flash, ui, FLASH_MS);
                self.redraw(ui);
            }
            Bell::Sound => {
                println!("TERM:BELL:sound");
                bell::play();
            }
            Bell::Notify => {
                // A toast for a window nobody is looking at.
                if !self.blink.focused() {
                    println!("TERM:BELL:notify");
                    bell::notify();
                }
            }
            Bell::None => {}
        }
    }

    /// The flash is over; its timer stops with it.
    pub(super) fn flash_off(&mut self, ui: &mut Ui<Msg>) {
        Timers::stop(&self.timers.flash, ui);
        self.chrome.borrow_mut().flash = false;
        self.redraw(ui);
    }
}
