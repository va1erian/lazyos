//! Host tests for the keymaps and the engine.
//!
//! The port-fidelity tests cross-check the compiled-in layouts against the
//! kernel's original scancode-keyed `layout.rs` data, translating through the
//! kernel's own set-1 to HID table (`#[path]`-included, so the two cannot
//! drift apart unnoticed).

use alloc::string::String;
use alloc::vec::Vec;

use crate::keymap::Layout;
use crate::{Engine, KeyOut, Output, RawKey};

#[path = "../../../../kernel/src/input/hid.rs"]
#[allow(dead_code, unexpected_cfgs)]
mod kernel_hid;

// HID usages used below.
const A: u16 = 0x04;
const Q: u16 = 0x14;
const ONE: u16 = 0x1E;
const ENTER: u16 = 0x28;
const TAB: u16 = 0x2B;
const SPACE: u16 = 0x2C;
const CAPS: u16 = 0x39;
const F4: u16 = 0x3D;
const NUM: u16 = 0x53;
const KP_1: u16 = 0x59;
const KP_0: u16 = 0x62;
const LCTRL: u16 = 0xE0;
const LSHIFT: u16 = 0xE1;
const LALT: u16 = 0xE2;
const LGUI: u16 = 0xE3;
const RALT: u16 = 0xE6;

struct Rig {
    engine: Engine,
    seq: u64,
    now: u64,
}

impl Rig {
    fn new(layout: Layout) -> Rig {
        Rig {
            engine: Engine::new(layout),
            seq: 0,
            now: 100,
        }
    }

    fn edge(&mut self, usage: u16, pressed: bool) -> Vec<Output> {
        self.seq += 1;
        let mut out = Vec::new();
        let raw = RawKey {
            seq: self.seq,
            ts_ns: self.now * crate::TICK_NS,
            usage,
            pressed,
        };
        self.engine.feed(raw, self.now, &mut out);
        out
    }

    fn down(&mut self, usage: u16) -> Vec<Output> {
        self.edge(usage, true)
    }

    fn up(&mut self, usage: u16) -> Vec<Output> {
        self.edge(usage, false)
    }

    /// Advance the clock and collect repeats.
    fn advance(&mut self, ticks: u64) -> Vec<Output> {
        self.now += ticks;
        let mut out = Vec::new();
        self.engine.tick(self.now, &mut out);
        out
    }
}

fn key(out: &[Output]) -> KeyOut {
    match out.first() {
        Some(Output::Key(key)) => *key,
        other => panic!("expected a key event, got {other:?}"),
    }
}

fn text(out: &[Output]) -> Option<String> {
    out.iter().find_map(|o| match o {
        Output::Text(text) => Some(text.clone()),
        _ => None,
    })
}

/// Type `usage` and return the character it produced, if any.
fn typed(rig: &mut Rig, usage: u16) -> Option<String> {
    let out = rig.down(usage);
    rig.up(usage);
    text(&out)
}

mod behavior;
mod console;
mod grab;
mod keymaps;
mod outbox;
mod pointer;
mod routing;
