//! Copy, paste and drops: the clipboard side of the Terminal. Pasted text is
//! sanitised by `lazyterm::paste`; a multi-line paste into a program that did
//! not ask for bracketed paste waits for a click or a key on a confirmation
//! bar, so a stray newline cannot run a command unseen.

use std::path::PathBuf;

use lazyterm::paste::{line_count, needs_confirmation, paste_bytes, shell_quote};
use std::cell::RefCell;
use std::rc::Rc;

use xui_app::backend::{DropEvent, LazyOSBackend};
use xui_app::platform::urilist;
use xui_app::probe;
use xui_core::app::Proxy;
use xui_core::app::Ui;
use xui_core::Rect;

use super::geometry::{confirm_rects, contains};
use super::pointer::{MouseKind, MouseMsg};
use super::{Msg, Terminal};
use xui_core::MouseButton;

/// Turn files dropped on the window into `Msg::Dropped` through `outbox`, which
/// holds the UI's proxy once the UI exists (the hook runs outside `update`).
pub(super) fn watch_drops(backend: &LazyOSBackend, outbox: Rc<RefCell<Option<Proxy<Msg>>>>) {
    backend.on_drag_event(move |_, event| {
        if let DropEvent::Drop {
            mime,
            data: Ok(bytes),
            ..
        } = event
        {
            if mime == urilist::MIME {
                if let Some(proxy) = outbox.borrow().as_ref() {
                    let _ = proxy.send(Msg::Dropped(urilist::decode(bytes)));
                }
            }
        }
    });
}

/// Whether a multi-line paste asks first (`paste/confirm_multiline`, a
/// setting from T3 on; always on until then).
const CONFIRM_MULTILINE: bool = true;
/// How many of a paste's last lines `TERM:CMD` reports.
const MAX_TRACKED_LINES: usize = 8;

impl Terminal {
    /// Copy the selection to the clipboard.
    pub(super) fn copy(&mut self, ui: &mut Ui<Msg>) {
        let text = self.term.borrow().selection_text();
        if let Some(text) = text {
            ui.set_clipboard_text(&text);
            println!("TERM:COPY:{}", text.len());
        }
    }

    pub(super) fn select_all(&mut self, ui: &mut Ui<Msg>) {
        self.term.borrow_mut().select_all();
        self.refresh(ui);
    }

    /// Paste the clipboard.
    pub(super) fn paste(&mut self, ui: &mut Ui<Msg>) {
        if let Some(text) = ui.clipboard_text() {
            self.paste_text(ui, text);
        }
    }

    fn paste_text(&mut self, ui: &mut Ui<Msg>, text: String) {
        if text.is_empty() {
            return;
        }
        let bracketed = self.term.borrow().bracketed_paste();
        if CONFIRM_MULTILINE && needs_confirmation(&text, bracketed) {
            let lines = line_count(&text);
            println!("TERM:PASTE:CONFIRM:{lines}");
            self.pending_text = Some(text);
            self.pending.set(Some(lines));
            self.show_confirm(ui);
        } else {
            self.write_paste(ui, &text);
        }
    }

    /// Send `text` to the program, and keep the typed-line bookkeeping (the
    /// `TERM:CMD` evidence) as if it had been typed.
    fn write_paste(&mut self, ui: &mut Ui<Msg>, text: &str) {
        let bracketed = self.term.borrow().bracketed_paste();
        let bytes = paste_bytes(text, bracketed);
        self.input(ui, &bytes);
        println!("TERM:PASTE:{}", bytes.len());
        if !bracketed {
            self.track_paste(&String::from_utf8_lossy(&bytes));
        }
    }

    /// Keep the typed-line bookkeeping (`TERM:CMD`) as if `text` was typed. A
    /// long paste reports only its last lines: the evidence is about the
    /// command the shell runs last, and a line of serial output per pasted
    /// line would cost more than the paste.
    fn track_paste(&mut self, text: &str) {
        let lines: Vec<&str> = text.split('\r').collect();
        for (at, line) in lines.iter().enumerate() {
            if at + MAX_TRACKED_LINES >= lines.len() {
                self.typed.push_str(line);
                if at + 1 < lines.len() {
                    self.submitted();
                }
            } else {
                self.typed.clear();
            }
        }
    }

    /// Files dropped on the window: their quoted paths, separated by spaces,
    /// with no newline (the user decides when to run it).
    pub(super) fn dropped(&mut self, ui: &mut Ui<Msg>, paths: Vec<PathBuf>) {
        let quoted: Vec<String> = paths
            .iter()
            .map(|path| shell_quote(&path.to_string_lossy()))
            .collect();
        if !quoted.is_empty() {
            self.write_paste(ui, &quoted.join(" "));
        }
    }

    /// The user answered the confirmation bar.
    pub(super) fn confirm(&mut self, ui: &mut Ui<Msg>, accept: bool) {
        let Some(text) = self.pending_text.take() else {
            return;
        };
        self.pending.set(None);
        self.hide_confirm(ui);
        if accept {
            self.write_paste(ui, &text);
        } else {
            println!("TERM:PASTE:CANCEL");
        }
    }

    /// A pointer event while the bar is up: only its buttons do anything.
    pub(super) fn confirm_click(&mut self, ui: &mut Ui<Msg>, m: &MouseMsg) {
        let (Some(layout), MouseKind::Down(MouseButton::Left) | MouseKind::Double(MouseButton::Left)) =
            (self.layout.get(), m.kind)
        else {
            return;
        };
        let rects = confirm_rects(&layout);
        if contains(rects.paste, m.x, m.y) {
            self.confirm(ui, true);
        } else if contains(rects.cancel, m.x, m.y) {
            self.confirm(ui, false);
        }
    }

    fn bar_rect(&self) -> Option<Rect> {
        self.layout.get().map(|layout| confirm_rects(&layout).bar)
    }

    fn show_confirm(&mut self, ui: &mut Ui<Msg>) {
        if let Some(layout) = self.layout.get() {
            let rects = confirm_rects(&layout);
            for (name, r) in [
                ("paste_confirm", rects.bar),
                ("paste_button", rects.paste),
                ("cancel_button", rects.cancel),
            ] {
                probe::widget("Terminal", name, r.left, r.top, r.width(), r.height());
            }
        }
        if let Some(bar) = self.bar_rect() {
            ui.invalidate_rect(self.root.id(), bar);
        }
    }

    fn hide_confirm(&mut self, ui: &mut Ui<Msg>) {
        if let Some(bar) = self.bar_rect() {
            ui.invalidate_rect(self.root.id(), bar);
        }
    }
}
