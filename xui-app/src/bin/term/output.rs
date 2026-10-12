//! The Terminal's two flows between the pty and the screen: input going out
//! (queued and written in chunks, because the kernel's line discipline drops
//! what it has no room for) and output coming in (drained, scrolled to, kept
//! on screen).

use std::io::{Read, Write};

use xui_core::app::Ui;

use super::blink::PERIOD_MS;
use super::evidence::is_prompt;
use super::scroll::{anchored, apply, Move};
use super::view::max_offset;
use super::{Msg, Terminal, FLUSH_MILLIS, INPUT_CHUNK};

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

    /// Run the timers that exist only while they are needed: the retry timer
    /// while input is queued, the blink timer while the cursor blinks. An idle
    /// Terminal runs neither, so it costs no frames.
    pub(super) fn pump(&mut self, ui: &mut Ui<Msg>) {
        match (self.out.is_empty(), self.flush.get()) {
            (false, None) => self.flush.set(Some(ui.set_timer(FLUSH_MILLIS))),
            (true, Some(id)) => {
                ui.kill_timer(id);
                self.flush.set(None);
            }
            _ => {}
        }
        match (self.blink.wants_timer(), self.blink_timer.get()) {
            (true, None) => self.blink_timer.set(Some(ui.set_timer(PERIOD_MS))),
            (false, Some(id)) => {
                ui.kill_timer(id);
                self.blink_timer.set(None);
            }
            _ => {}
        }
    }

    /// Write what the user typed: the view returns to the bottom first.
    pub(super) fn input(&mut self, ui: &mut Ui<Msg>, bytes: &[u8]) {
        self.scroll(ui, Move::Bottom);
        self.send(bytes);
    }

    /// Repaint what changed on screen (the rows, or the scrollbar), unless a
    /// synchronized update is open (`meta.rs` holds it for a moment).
    pub(super) fn refresh(&mut self, ui: &mut Ui<Msg>) {
        if self.hold_for_sync(ui) {
            return;
        }
        self.repaint(ui);
    }

    /// The repaint itself.
    pub(super) fn repaint(&mut self, ui: &mut Ui<Msg>) {
        let key = self.look.borrow().key();
        let changed = self
            .shown
            .update(&self.term.borrow(), self.layout.get(), self.offset.get(), key);
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
        self.layout
            .get()
            .map_or(self.term.borrow().rows(), |layout| layout.visible)
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

    /// Output arrived: feed the engine, answer its queries, report the
    /// evidence, repaint what changed.
    pub(super) fn output(&mut self, ui: &mut Ui<Msg>, bytes: &[u8]) {
        let (top, completed, reply) = {
            let mut term = self.term.borrow_mut();
            let top = term.screen_top();
            (top, term.feed(bytes), term.take_reply())
        };
        self.follow(top);
        // Answers to cursor-position and device-attribute queries go back to
        // the program; ones that wait for them hang.
        if !reply.is_empty() {
            self.send(&reply);
        }
        for line in &completed {
            if let Some(marker) = self.evidence.line(line) {
                println!("TERM:OUT:{marker}");
            }
        }
        self.touch(ui);
        self.meta_changed(ui);
        self.refresh(ui);
        let at_prompt = {
            let term = self.term.borrow();
            let row = term.row_text(term.cursor().0);
            !row.is_empty() && is_prompt(&row)
        };
        self.perf.read(bytes.len(), at_prompt, &self.paint_clock);
    }
}
