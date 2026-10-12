//! The line discipline: what a terminal does between the keys typed and the
//! bytes a program reads, driven by [`Termios`].
//!
//! Canonical mode (`ICANON`) collects a line with editing (`VERASE`, `VKILL`,
//! `VWERASE`, `VLNEXT`), echoes it (`ECHO`, `ECHOE`, `ECHOCTL`), and hands it
//! over a line per `read` once it ends (`\n`, `VEOL`, or `VEOF`, which on an
//! empty line is end-of-file). Raw mode passes bytes through as they come,
//! with `VMIN`/`VTIME` deciding how long a read waits (the caller's job).
//! `ISIG` turns `VINTR`/`VQUIT`/`VSUSP` into signals for the foreground group.
//! Input mapping (`ICRNL`, `INLCR`, `IGNCR`) applies in both modes, and
//! output post-processing (`OPOST`+`ONLCR`) turns `\n` into `\r\n`.
//!
//! The type is pure: no locks, no tasks. The console terminal and the
//! pseudo-terminals ([`super::pty`]) each own one.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use super::termios::*;

/// Most bytes of input held at once (Linux's `N_TTY_BUF_SIZE`). A pty master
/// writer waits for room ([`Ldisc::admits`]); the console's keys past it are
/// dropped. In canonical mode the line being edited can always be ended.
pub const MAX_INPUT: usize = 4096;

/// A signal the discipline asks the terminal's owner to raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Quit,
    Suspend,
}

/// A terminal's foreground group with the session it was set in: a signal
/// the terminal raises goes to the group only while it is still in that
/// session ([`crate::tty::signal_foreground`]), so a recycled group id in
/// another session never receives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Foreground {
    pub group: usize,
    pub session: usize,
}

/// One terminal's input state.
#[derive(Clone, Debug, Default)]
pub struct Ldisc {
    pub termios: Termios,
    pub winsize: WinSize,
    /// The foreground process group (`TIOCSPGRP`); 0 = not set yet.
    pub fg_pgrp: usize,
    /// The session whose controlling terminal this is; 0 = none. Only a
    /// pseudo-terminal records one (a console's session is its window's).
    pub session: usize,
    /// The line being edited (canonical mode).
    edit: Vec<u8>,
    /// Bytes a `read` can take.
    ready: VecDeque<u8>,
    /// Canonical mode: the lengths of the complete lines in `ready`, oldest
    /// first; a 0 is an end-of-file mark (`VEOF` on an empty line).
    lines: VecDeque<usize>,
    /// The next byte is literal (`VLNEXT`).
    literal: bool,
}

impl Ldisc {
    pub fn new() -> Ldisc {
        Ldisc::default()
    }

    /// Who the terminal's own signals (`^C`, `SIGWINCH`, `SIGHUP`) are for.
    pub fn foreground(&self) -> Foreground {
        Foreground {
            group: self.fg_pgrp,
            session: self.session,
        }
    }

    /// Whether a `read` would return now (data, or end-of-file).
    pub fn readable(&self) -> bool {
        if self.termios.canonical() {
            !self.lines.is_empty()
        } else {
            !self.ready.is_empty()
        }
    }

    /// Bytes a `read` could take now (`FIONREAD`).
    pub fn available(&self) -> usize {
        if self.termios.canonical() {
            self.lines.front().copied().unwrap_or(0)
        } else {
            self.ready.len()
        }
    }

    /// Discard pending input (`TCFLSH`, `TCSETSF`, an interrupt).
    pub fn flush_input(&mut self) {
        self.edit.clear();
        self.ready.clear();
        self.lines.clear();
        self.literal = false;
    }

    /// Install new settings. Leaving canonical mode makes the half-edited line
    /// readable as it is; entering it makes the queued raw bytes one line.
    pub fn set_termios(&mut self, termios: Termios) {
        let was = self.termios.canonical();
        self.termios = termios;
        match (was, termios.canonical()) {
            (true, false) => {
                self.ready.extend(self.edit.drain(..));
                self.lines.clear();
            }
            (false, true) if !self.ready.is_empty() => {
                self.lines.clear();
                self.lines.push_back(self.ready.len());
            }
            _ => {}
        }
    }

    /// Take up to `dst.len()` bytes: at most one line in canonical mode
    /// (0 for an end-of-file mark), anything queued in raw mode.
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let limit = if self.termios.canonical() {
            let Some(&line) = self.lines.front() else {
                return 0;
            };
            if line == 0 {
                self.lines.pop_front();
                return 0;
            }
            line
        } else {
            self.ready.len()
        };
        let n = dst.len().min(limit);
        for slot in dst.iter_mut().take(n) {
            *slot = self.ready.pop_front().unwrap_or(0);
        }
        if self.termios.canonical() {
            if n == limit {
                self.lines.pop_front();
            } else if let Some(front) = self.lines.front_mut() {
                *front -= n;
            }
        }
        n
    }

    /// Whether an ordinary byte would be taken now ([`Ldisc::admits`]): a pty
    /// master's `POLLOUT`.
    pub fn writable(&self) -> bool {
        if self.termios.canonical() {
            self.lines.is_empty() || self.used() < MAX_INPUT - 1
        } else {
            self.ready.len() < MAX_INPUT
        }
    }

    /// Whether typed `byte` can be fed now without being lost for want of
    /// room; `false` means the writer waits for a read, which can drain what
    /// is queued. Bytes that need no room (signals, editing, an ignored CR)
    /// are always admitted, and so is anything while canonical input holds no
    /// complete line: no read could make room, so an overlong line drops its
    /// excess and its end still fits (Linux's rule).
    pub fn admits(&self, byte: u8) -> bool {
        let t = self.termios;
        let Some(byte) = self.map_input(byte) else {
            return true;
        };
        if !self.literal && self.signal_for(byte).is_some() {
            return true;
        }
        if !t.canonical() {
            return self.ready.len() < MAX_INPUT;
        }
        if self.lines.is_empty() {
            return true;
        }
        let used = self.used();
        if self.literal {
            return used < MAX_INPUT - 1;
        }
        let extended = t.lflag & IEXTEN != 0;
        if t.is_cc(VERASE, byte)
            || t.is_cc(VKILL, byte)
            || (extended && (t.is_cc(VWERASE, byte) || t.is_cc(VLNEXT, byte)))
        {
            true
        } else if t.is_cc(VEOF, byte) {
            // Moves no byte, but each end-of-file mark is a queued line.
            self.lines.len() < MAX_INPUT
        } else if byte == b'\n' || t.is_cc(VEOL, byte) || (extended && t.is_cc(VEOL2, byte)) {
            used < MAX_INPUT
        } else {
            used < MAX_INPUT - 1
        }
    }

    /// Bytes held: the line being edited and those waiting for a read.
    fn used(&self) -> usize {
        self.edit.len() + self.ready.len()
    }

    /// Input mapping (`IGNCR`, `ICRNL`, `INLCR`); `None` drops the byte.
    fn map_input(&self, byte: u8) -> Option<u8> {
        let iflag = self.termios.iflag;
        match byte {
            b'\r' if iflag & IGNCR != 0 => None,
            b'\r' if iflag & ICRNL != 0 => Some(b'\n'),
            b'\n' if iflag & INLCR != 0 => Some(b'\r'),
            other => Some(other),
        }
    }

    /// The signal `byte` raises under `ISIG`, if any.
    fn signal_for(&self, byte: u8) -> Option<Signal> {
        let t = self.termios;
        if !t.signals() {
            None
        } else if t.is_cc(VINTR, byte) {
            Some(Signal::Interrupt)
        } else if t.is_cc(VQUIT, byte) {
            Some(Signal::Quit)
        } else if t.is_cc(VSUSP, byte) {
            Some(Signal::Suspend)
        } else {
            None
        }
    }

    /// Feed one typed byte. Echo goes to `echo` (already post-processed for
    /// output); a returned signal is for the foreground group.
    pub fn input(&mut self, byte: u8, echo: &mut Vec<u8>) -> Option<Signal> {
        let byte = self.map_input(byte)?;
        if self.literal {
            self.literal = false;
            self.insert(byte, echo);
            return None;
        }
        if let Some(signal) = self.signal_for(byte) {
            self.flush_input();
            self.echo_char(byte, echo);
            return Some(signal);
        }
        if !self.termios.canonical() {
            if self.ready.len() < MAX_INPUT {
                self.ready.push_back(byte);
                self.echo_char(byte, echo);
            }
            return None;
        }
        self.canonical_input(byte, echo);
        None
    }

    /// The editing characters and line ends of canonical mode.
    fn canonical_input(&mut self, byte: u8, echo: &mut Vec<u8>) {
        let t = self.termios;
        let extended = t.lflag & IEXTEN != 0;
        if t.is_cc(VERASE, byte) {
            self.erase(echo);
        } else if t.is_cc(VKILL, byte) {
            while !self.edit.is_empty() {
                self.erase(echo);
            }
        } else if extended && t.is_cc(VWERASE, byte) {
            while self.edit.last() == Some(&b' ') {
                self.erase(echo);
            }
            while self.edit.last().is_some_and(|&c| c != b' ') {
                self.erase(echo);
            }
        } else if extended && t.is_cc(VLNEXT, byte) {
            self.literal = true;
        } else if t.is_cc(VEOF, byte) {
            self.end_line();
        } else if byte == b'\n' || t.is_cc(VEOL, byte) || (extended && t.is_cc(VEOL2, byte)) {
            self.edit.push(byte);
            if t.echo() || t.lflag & ECHONL != 0 {
                self.output(&[byte], echo);
            }
            self.end_line();
        } else {
            self.insert(byte, echo);
        }
    }

    /// Add `byte` to the line being edited (or the raw queue) and echo it.
    fn insert(&mut self, byte: u8, echo: &mut Vec<u8>) {
        if !self.termios.canonical() {
            if self.ready.len() < MAX_INPUT {
                self.ready.push_back(byte);
                self.echo_char(byte, echo);
            }
            return;
        }
        // Keep room for the line end, as Linux does.
        if self.edit.len() + self.ready.len() < MAX_INPUT - 1 {
            self.edit.push(byte);
            self.echo_char(byte, echo);
        }
    }

    /// Move the edited line to the readable queue (`VEOF` or a line end).
    fn end_line(&mut self) {
        let len = self.edit.len();
        self.ready.extend(self.edit.drain(..));
        self.lines.push_back(len);
    }

    /// Remove the last edited character, rubbing it out on the screen.
    fn erase(&mut self, echo: &mut Vec<u8>) {
        let Some(gone) = self.edit.pop() else {
            return;
        };
        let t = self.termios;
        if t.echo() && t.lflag & ECHOE != 0 {
            let width = if gone < 0x20 && gone != b'\t' && t.lflag & ECHOCTL != 0 {
                2
            } else {
                1
            };
            for _ in 0..width {
                echo.extend_from_slice(b"\x08 \x08");
            }
        }
    }

    /// Echo one input byte: printable as itself, a control character as
    /// `^X` under `ECHOCTL` (tab and newline as themselves).
    fn echo_char(&self, byte: u8, echo: &mut Vec<u8>) {
        let t = self.termios;
        if !t.echo() {
            return;
        }
        if byte < 0x20 && byte != b'\t' && byte != b'\n' && t.lflag & ECHOCTL != 0 {
            echo.push(b'^');
            echo.push(byte ^ 0x40);
        } else if byte == 0x7f && t.lflag & ECHOCTL != 0 {
            echo.extend_from_slice(b"^?");
        } else {
            self.output(&[byte], echo);
        }
    }

    /// Output post-processing: with `OPOST|ONLCR`, `\n` becomes `\r\n`.
    pub fn output(&self, src: &[u8], out: &mut Vec<u8>) {
        let crlf = self.termios.oflag & (OPOST | ONLCR) == OPOST | ONLCR;
        for &byte in src {
            if byte == b'\n' && crlf {
                out.push(b'\r');
            }
            out.push(byte);
        }
    }
}
