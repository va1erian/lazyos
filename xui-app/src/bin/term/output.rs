//! The Terminal's two flows between the pty and the screen: input going out
//! (queued and written in chunks, because the kernel's line discipline drops
//! what it has no room for) and output coming in (drained, scrolled to, kept
//! on screen).

use std::io::{Read, Write};

use xui_core::app::Ui;

use super::scroll::{anchored, apply, Move};
use super::view::max_offset;
use super::{Msg, Terminal, FLUSH_MILLIS, INPUT_CHUNK, ROWS};

impl Terminal {
    /// Write to the pty without touching the view (answers to queries). What
    /// the pty has no room for stays queued (`flush_out`, `pump`).
    pub(super) fn send(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
        self.flush_out();
    }

    /// Write the next chunk of the queue. The kernel's line discipline drops
    /// input it has no room for (4096 bytes) instead of pushing back on the
    /// writer, so a big paste goes in `INPUT_CHUNK` bytes at a time, one per
    /// `FLUSH_MILLIS`, and the shell reads each before the next arrives.
    pub(super) fn flush_out(&mut self) {
        while !self.out.is_empty() {
            let chunk = self.out.len().min(INPUT_CHUNK);
            match self.master.file().write(&self.out[..chunk]) {
                Ok(0) => break,
                Ok(n) => {
                    self.out.drain(..n);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // The shell is gone: the drain notices; there is no one to write to.
                Err(_) => self.out.clear(),
            }
        }
    }

    /// Run the retry timer while input is queued, and not otherwise.
    pub(super) fn pump(&mut self, ui: &mut Ui<Msg>) {
        match (self.out.is_empty(), self.flush.get()) {
            (false, None) => self.flush.set(Some(ui.set_timer(FLUSH_MILLIS))),
            (true, Some(id)) => {
                ui.kill_timer(id);
                self.flush.set(None);
            }
            _ => {}
        }
    }

    /// Write what the user typed: the view returns to the bottom first.
    pub(super) fn input(&mut self, ui: &mut Ui<Msg>, bytes: &[u8]) {
        self.scroll(ui, Move::Bottom);
        self.send(bytes);
    }

    /// Repaint what changed on screen (the rows, or the scrollbar).
    pub(super) fn refresh(&mut self, ui: &mut Ui<Msg>) {
        let changed = self
            .shown
            .update(&self.term.borrow(), self.layout.get(), self.offset.get());
        if let Some(rect) = changed {
            ui.invalidate_rect(self.root.id(), rect);
        }
    }

    /// Move the view on the user's request: `TERM:SCROLL:<offset>` when it
    /// changes. A full-screen program has no scrollback, so nothing moves.
    pub(super) fn scroll(&mut self, ui: &mut Ui<Msg>, mv: Move) {
        let max = max_offset(&self.term.borrow(), self.page());
        let offset = apply(mv, self.offset.get(), max);
        if offset != self.offset.get() {
            self.offset.set(offset);
            println!("TERM:SCROLL:{offset}");
            self.refresh(ui);
        }
    }

    /// Keep the view on its text while output arrives: `top` was the
    /// screen's top line before it.
    pub(super) fn follow(&mut self, top: u64) {
        let term = self.term.borrow();
        let max = max_offset(&term, self.page());
        self.offset
            .set(anchored(self.offset.get(), top, term.screen_top(), max));
    }

    /// The lines a page scroll moves: what the window shows.
    pub(super) fn page(&self) -> usize {
        self.layout.get().map_or(ROWS, |layout| layout.visible)
    }

    /// Drain the master without blocking; return the bytes read.
    pub(super) fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        while !self.hung_up {
            match self.master.file().read(&mut chunk) {
                Ok(0) => self.hung_up = true,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // `EIO`: every slave descriptor closed, the shell is gone.
                Err(_) => self.hung_up = true,
            }
        }
        out
    }
}
