//! Serial evidence lines (`trace=1`): what `inputd` decoded, so a scripted
//! QEMU session can prove both layouts type the right characters without
//! reading pixels. Off in the desktop profile.

use alloc::format;

use inputmap::{KeyState, Layout, Output, PointerOut};
use user::sys;

pub(super) struct Trace {
    on: bool,
}

impl Trace {
    /// `trace=1` among the service arguments turns it on.
    pub(super) fn from_args() -> Trace {
        let mut buf = [0u8; 64];
        let len = sys::service_args(&mut buf);
        let args = core::str::from_utf8(&buf[..len.min(buf.len())]).unwrap_or("");
        Trace {
            on: args.split_whitespace().any(|arg| arg == "trace=1"),
        }
    }

    pub(super) fn layout(&self, layout: Layout) {
        sys::write_str(&format!("INPUTD:LAYOUT {}\n", layout.name()));
    }

    pub(super) fn outputs(&self, outputs: &[Output]) {
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
                    sys::write_str(&format!(
                        "INPUTD:KEY code={:#x} sym={:#x} mods={:#x} {state}\n",
                        key.code, key.sym, key.mods
                    ));
                }
                Output::Text(text) => {
                    let scalar = text.chars().next().map_or(0, |c| c as u32);
                    sys::write_str(&format!("INPUTD:TEXT u+{scalar:x}\n"));
                }
                Output::Hotkey(id) => sys::write_str(&format!("INPUTD:HOTKEY {id}\n")),
            }
        }
    }

    pub(super) fn pointer(&self, outputs: &[PointerOut]) {
        if !self.on {
            return;
        }
        for out in outputs {
            sys::write_str(&format!(
                "INPUTD:POINTER x={} y={} buttons={:#x} wheel={},{}\n",
                out.x, out.y, out.buttons, out.wheel_v, out.wheel_h
            ));
        }
    }
}
