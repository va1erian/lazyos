//! Serial evidence lines (`trace=1`): what `inputd` decoded, so a scripted
//! QEMU session can prove both layouts type the right characters without
//! reading pixels. Off in the desktop profile.
//!
//! **Deferred output** (issue #400). The serial port drains about 11 KB/s, and
//! a burst of injected keys produces trace lines faster than that: writing
//! them inline kept `inputd` away from the raw bus long enough for its
//! 256-event ring to overflow (`INPUTD:RESYNC`), so the trace itself lost the
//! keys it was meant to prove. Lines are queued instead and written a bounded
//! chunk per pass of the service loop, after the bus has been drained, so the
//! bus never waits on the serial port.

use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use inputmap::{KeyState, Layout, Output, PointerOut};
use user::sys;

/// Most bytes written per loop pass (two trace lines).
const CHUNK: usize = 128;
/// Most bytes queued; past this, lines are counted and dropped (with a note)
/// rather than letting a flood grow the heap without bound.
const MAX_QUEUED: usize = 1 << 20;

pub(super) struct Trace {
    on: bool,
    queued: String,
    /// Lines dropped since the last note.
    lost: u64,
}

impl Trace {
    /// `trace=1` among the service arguments turns it on.
    pub(super) fn from_args() -> Trace {
        let mut buf = [0u8; 64];
        let len = sys::service_args(&mut buf);
        let args = core::str::from_utf8(&buf[..len.min(buf.len())]).unwrap_or("");
        Trace {
            on: args.split_whitespace().any(|arg| arg == "trace=1"),
            queued: String::new(),
            lost: 0,
        }
    }

    /// Queue one line (with its newline).
    fn line(&mut self, args: core::fmt::Arguments) {
        if self.queued.len() >= MAX_QUEUED {
            self.lost += 1;
            return;
        }
        if self.lost > 0 {
            let _ = writeln!(self.queued, "INPUTD:TRACE:LOST lines={}", self.lost);
            self.lost = 0;
        }
        let _ = self.queued.write_fmt(args);
        self.queued.push('\n');
    }

    /// Whether lines wait for the serial port (the loop then comes back soon).
    pub(super) fn pending(&self) -> bool {
        !self.queued.is_empty()
    }

    /// Write the next chunk of queued lines, ending on a line boundary.
    pub(super) fn flush(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let end = if self.queued.len() <= CHUNK {
            self.queued.len()
        } else {
            // Whole lines only, so a reader never sees half of one.
            self.queued[..CHUNK].rfind('\n').map_or(CHUNK, |at| at + 1)
        };
        let end = (0..=end)
            .rev()
            .find(|&at| self.queued.is_char_boundary(at))
            .unwrap_or(0);
        sys::write_str(&self.queued[..end]);
        self.queued.drain(..end);
    }

    pub(super) fn layout(&mut self, layout: Layout) {
        self.line(format_args!("INPUTD:LAYOUT {}", layout.name()));
    }

    pub(super) fn outputs(&mut self, outputs: &[Output]) {
        if !self.on {
            return;
        }
        for output in outputs {
            match output {
                Output::Key(key) => {
                    let state = match key.state {
                        KeyState::Down => "down",
                        KeyState::Up => "up",
                        KeyState::Repeat => "repeat",
                    };
                    // `t=`: the tick it was decoded, to place a late key;
                    // `seq=`: the raw bus number, so a gap shows where a
                    // key was lost (before the bus or after it).
                    self.line(format_args!(
                        "INPUTD:KEY code={:#x} sym={:#x} mods={:#x} {state} t={} seq={}",
                        key.code,
                        key.sym,
                        key.mods,
                        sys::clock(),
                        key.seq
                    ));
                }
                Output::Text(text) => {
                    let scalar = text.chars().next().map_or(0, |c| c as u32);
                    self.line(format_args!("INPUTD:TEXT u+{scalar:x}"));
                }
                Output::Hotkey(id) => self.line(format_args!("INPUTD:HOTKEY {id}")),
            }
        }
    }

    pub(super) fn pointer(&mut self, outputs: &[PointerOut]) {
        if !self.on {
            return;
        }
        for out in outputs {
            self.line(format_args!(
                "INPUTD:POINTER x={} y={} buttons={:#x} wheel={},{}",
                out.x, out.y, out.buttons, out.wheel_v, out.wheel_h
            ));
        }
    }
}

/// The raw ring overflowed. Always logged at once, trace or not: loss is
/// never silent, and the held keys are released (`Engine::resync`) so none
/// sticks.
pub(super) fn dropped(seq: u64, lost: u64) {
    sys::write_str(&format!("INPUTD:RESYNC seq={seq} lost={lost}\n"));
}
