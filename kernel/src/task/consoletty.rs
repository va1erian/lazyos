//! The console terminal's line discipline: the [`Ldisc`] of a kernel window,
//! kept with the window's root task next to its key queue.
//!
//! Keys stay in the root task's queue until someone reads (or polls) the
//! terminal; then [`with_console`] converts them to the bytes a VT100-style
//! terminal sends (arrows as `ESC [ A`, Enter as `\r`, Backspace as `DEL`),
//! runs them through the discipline, and hands back what it echoed and which
//! signals it raised, for the caller to deliver once the task table is
//! unlocked. `^C` itself is turned into `SIGINT` by the keyboard path
//! ([`super::on_key`]) as soon as it is typed, unless the terminal's `ISIG`
//! is off, in which case it is queued as an ordinary byte.

use alloc::boxed::Box;

use crate::tty::{Foreground, Ldisc, Signal};

use super::*;

/// What feeding the discipline produced besides its own state change.
#[derive(Default)]
pub struct Fed {
    /// Echo for the terminal's output.
    pub echo: Vec<u8>,
    /// Signals to raise, with the group and session they are for.
    pub signals: Vec<(Signal, Foreground)>,
}

/// The bytes a terminal sends for `key` (none for a bare modifier).
pub fn key_bytes(key: Key, out: &mut Vec<u8>) {
    let seq: &[u8] = match key {
        Key::Char(c) => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            return;
        }
        Key::Enter => b"\r",
        Key::Space => b" ",
        Key::Tab => b"\t",
        Key::Backspace => b"\x7f",
        Key::Escape => b"\x1b",
        Key::Up => b"\x1b[A",
        Key::Down => b"\x1b[B",
        Key::Right => b"\x1b[C",
        Key::Left => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::Insert => b"\x1b[2~",
        Key::Delete => b"\x1b[3~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Shift | Key::Ctrl | Key::Alt | Key::Super | Key::F(_) => b"",
    };
    out.extend_from_slice(seq);
}

/// Feed the current terminal's pending keys into its discipline, then run `f`
/// on it. The discipline is created (`stty sane`) on first use.
pub fn with_console<R>(f: impl FnOnce(&mut Ldisc) -> R) -> (R, Fed) {
    let mut fed = Fed::default();
    let mut tasks = TASKS.lock();
    let root = root_index(&tasks);
    let Some(task) = tasks[root].as_mut() else {
        let mut scratch = Ldisc::new();
        return (f(&mut scratch), fed);
    };
    let (pgid, session) = (task.pgid, task.sid);
    let ldisc = task
        .linux
        .console
        .get_or_insert_with(|| Box::new(Ldisc::new()));
    let mut bytes = Vec::new();
    while let Some(key) = task.input.pop_front() {
        key_bytes(key, &mut bytes);
    }
    for byte in bytes {
        if let Some(signal) = ldisc.input(byte, &mut fed.echo) {
            let group = if ldisc.fg_pgrp != 0 {
                ldisc.fg_pgrp
            } else {
                pgid
            };
            fed.signals.push((signal, Foreground { group, session }));
        }
    }
    (f(ldisc), fed)
}

/// The session the current task's console belongs to: its window's root
/// task's (the session a console window's job control runs in).
pub fn console_session() -> usize {
    let tasks = TASKS.lock();
    let root = root_index(&tasks);
    tasks[root].as_ref().map_or(0, |task| task.sid)
}

/// Deliver what [`with_console`] produced (call with the table unlocked).
pub fn apply_fed(fed: Fed) {
    if !fed.echo.is_empty() {
        // The window renderer starts a line on `\n` and takes a bare `\r` as
        // "rewrite this line", so the `\r` of an echoed `\r\n` would erase the
        // line just typed: drop it here.
        let mut echo = fed.echo;
        let mut index = 0;
        while index + 1 < echo.len() {
            if echo[index] == b'\r' && echo[index + 1] == b'\n' {
                echo.remove(index);
            } else {
                index += 1;
            }
        }
        write_output(&echo);
        crate::serial::write_bytes(&echo);
    }
    for (signal, foreground) in fed.signals {
        crate::tty::signal_console(foreground, crate::tty::signal_number(signal));
    }
}

/// Whether a read of the current terminal would return now.
pub fn console_readable() -> bool {
    let (ready, fed) = with_console(|ldisc| ldisc.readable());
    apply_fed(fed);
    ready
}

/// The keyboard path's view of the focused window's terminal: whether `^C`
/// should become a signal (`ISIG`), and for whom: the foreground group
/// `TIOCSPGRP` set, in the window's session, else the focused task's own
/// group in its own session.
pub fn console_interrupt_target(focus: usize) -> Option<Foreground> {
    let tasks = TASKS.lock();
    let root = super::console::root_of(&tasks, focus);
    let focused = tasks[focus].as_ref().map_or(
        Foreground {
            group: 0,
            session: 0,
        },
        |task| Foreground {
            group: task.pgid,
            session: task.sid,
        },
    );
    let root = tasks[root].as_ref();
    match root.and_then(|task| task.linux.console.as_deref()) {
        Some(ldisc) if !ldisc.termios.signals() => None,
        Some(ldisc) if ldisc.fg_pgrp != 0 => Some(Foreground {
            group: ldisc.fg_pgrp,
            session: root.map_or(0, |task| task.sid),
        }),
        _ => Some(focused),
    }
}
