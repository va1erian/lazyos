//! The input state machine: physical key edges in, logical events out.

use alloc::string::String;
use alloc::vec::Vec;

use crate::keymap::{self, Layout};
use crate::mods;
use crate::repeat::{Repeater, TICK_NS};

/// One physical key edge from the kernel bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawKey {
    /// The kernel's global sequence number for this event.
    pub seq: u64,
    pub ts_ns: u64,
    /// USB HID usage, page 0x07.
    pub usage: u16,
    pub pressed: bool,
}

/// Press, release or auto-repeat (never confused with each other).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    Down,
    Up,
    Repeat,
}

/// A logical key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyOut {
    /// The physical key (HID usage): always present, layout-independent.
    pub code: u16,
    /// Its meaning under the active layout, `0` when the key has none on the
    /// current level. See [`crate::keysym`]. With Ctrl held a letter reports
    /// its unshifted form, so shortcuts do not depend on Shift or Caps Lock.
    pub sym: u32,
    /// The modifier and lock bits ([`crate::mods`]) in force *after* this event
    /// took effect (a Shift press reports `SHIFT`, its release does not).
    pub mods: u32,
    pub state: KeyState,
    pub ts_ns: u64,
    /// The raw sequence number of the edge that caused it (repeats carry that
    /// of the most recent raw event).
    pub seq: u64,
}

/// What the engine produced for one input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Key(KeyOut),
    /// Composed text for a character-producing press or repeat (never for
    /// Ctrl/Alt/Super chords).
    Text(String),
    /// A registered hotkey matched; the key event is consumed, not delivered.
    Hotkey(u64),
    /// The reserved escape chord ([`ESCAPE_CODE`] with exactly
    /// [`ESCAPE_MODS`]) was pressed: any keyboard grab must end. Consumed
    /// like a hotkey, grab or not, and never registrable.
    Escape,
}

/// The escape chord's key: Escape (HID usage 0x29).
pub const ESCAPE_CODE: u16 = 0x29;
/// The escape chord's modifiers: Ctrl+Alt.
pub const ESCAPE_MODS: u32 = mods::CTRL | mods::ALT;

struct Hotkey {
    id: u64,
    code: u16,
    mods: u32,
}

/// Modifier keys, lock keys and the like never repeat.
fn repeatable(usage: u16) -> bool {
    !matches!(usage, 0xE0..=0xE7 | 0x39 | 0x47 | 0x53 | 0x46 | 0x48)
}

pub struct Engine {
    layout: Layout,
    /// One bit per HID usage 0..=255.
    down: [u64; 4],
    /// Presses a hotkey consumed: their release is swallowed too.
    consumed: [u64; 4],
    locks: u32,
    repeater: Repeater,
    hotkeys: Vec<Hotkey>,
    next_hotkey: u64,
    last_seq: u64,
    /// A session holds a keyboard grab: no hotkey matches (the escape chord
    /// still does).
    grabbed: bool,
}

fn bit(usage: u16) -> (usize, u64) {
    ((usage as usize >> 6) & 3, 1u64 << (usage & 63))
}

fn test(map: &[u64; 4], usage: u16) -> bool {
    let (word, mask) = bit(usage);
    map[word] & mask != 0
}

fn set(map: &mut [u64; 4], usage: u16, on: bool) {
    let (word, mask) = bit(usage);
    if on {
        map[word] |= mask;
    } else {
        map[word] &= !mask;
    }
}

impl Engine {
    /// A fresh engine: nothing held, NumLock on (the usual firmware default).
    pub fn new(layout: Layout) -> Engine {
        Engine {
            layout,
            down: [0; 4],
            consumed: [0; 4],
            locks: mods::NUM_LOCK,
            repeater: Repeater::default(),
            hotkeys: Vec::new(),
            next_hotkey: 1,
            last_seq: 0,
            grabbed: false,
        }
    }

    /// A keyboard grab began (`true`) or ended: while one is held, presses
    /// skip the hotkey table and go to the grabbing session.
    pub fn set_grabbed(&mut self, grabbed: bool) {
        self.grabbed = grabbed;
    }

    /// Every key held, one bit per HID usage (bit `u & 63` of word `u >> 6`),
    /// as the key-state page carries it.
    pub fn down_bits(&self) -> [u64; 4] {
        self.down
    }

    /// The raw sequence number of the newest event processed.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Whether `code` with chord `mods` is the reserved escape chord.
    pub fn is_reserved(code: u16, chord: u32) -> bool {
        code == ESCAPE_CODE && chord & mods::CHORD_MASK == ESCAPE_MODS
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Switch layout live. Held keys keep their state; repeat is cancelled so
    /// a repeating key cannot change character mid-stream.
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        self.repeater.cancel();
    }

    /// Register a chord: `code` with exactly the `mods` ([`mods::CHORD_MASK`]
    /// bits) held. Returns its id, or `None` for the reserved escape chord.
    pub fn add_hotkey(&mut self, code: u16, chord: u32) -> Option<u64> {
        if Engine::is_reserved(code, chord) {
            return None;
        }
        let id = self.next_hotkey;
        self.next_hotkey += 1;
        self.hotkeys.push(Hotkey {
            id,
            code,
            mods: chord & mods::CHORD_MASK,
        });
        Some(id)
    }

    /// How many chords are registered.
    pub fn hotkey_count(&self) -> usize {
        self.hotkeys.len()
    }

    /// Remove a chord; whether it existed.
    pub fn remove_hotkey(&mut self, id: u64) -> bool {
        let before = self.hotkeys.len();
        self.hotkeys.retain(|hotkey| hotkey.id != id);
        self.hotkeys.len() != before
    }

    /// The modifier and lock bits right now.
    pub fn mods(&self) -> u32 {
        let held = |usage| test(&self.down, usage);
        let mut bits = self.locks;
        if held(0xE1) || held(0xE5) {
            bits |= mods::SHIFT;
        }
        if held(0xE0) || held(0xE4) {
            bits |= mods::CTRL;
        }
        if held(0xE2) {
            bits |= mods::ALT;
        }
        if held(0xE6) {
            bits |= if self.layout.has_altgr() {
                mods::ALTGR
            } else {
                mods::ALT
            };
        }
        if held(0xE3) || held(0xE7) {
            bits |= mods::SUPER;
        }
        bits
    }

    /// Every key currently held, in usage order.
    pub fn held(&self) -> Vec<u16> {
        (0..=255u16)
            .filter(|&usage| test(&self.down, usage))
            .collect()
    }

    /// Cancel any pending repeat (focus change, grab change).
    pub fn cancel_repeat(&mut self) {
        self.repeater.cancel();
    }

    /// The tick the next repeat is due, for sleeping exactly until then.
    pub fn next_due(&self) -> Option<u64> {
        self.repeater.next_due()
    }

    /// Process one raw edge at PIT tick `now`.
    pub fn feed(&mut self, raw: RawKey, now: u64, out: &mut Vec<Output>) {
        self.last_seq = raw.seq;
        if raw.pressed {
            self.press(raw, now, out);
        } else {
            self.release(raw, out);
        }
    }

    fn press(&mut self, raw: RawKey, now: u64, out: &mut Vec<Output>) {
        let usage = raw.usage;
        if test(&self.down, usage) {
            // The kernel already swallows typematic; a stray duplicate is not
            // a new press.
            return;
        }
        let before = self.mods() & mods::CHORD_MASK;
        set(&mut self.down, usage, true);
        match usage {
            0x39 => self.locks ^= mods::CAPS_LOCK,
            0x53 => self.locks ^= mods::NUM_LOCK,
            0x47 => self.locks ^= mods::SCROLL_LOCK,
            _ => {}
        }
        // The escape chord first: no grab and no registration can shadow it.
        if Engine::is_reserved(usage, before) {
            set(&mut self.consumed, usage, true);
            out.push(Output::Escape);
            return;
        }
        if let Some(hotkey) = self
            .hotkeys
            .iter()
            .filter(|_| !self.grabbed)
            .find(|hotkey| hotkey.code == usage && hotkey.mods == before)
        {
            let id = hotkey.id;
            set(&mut self.consumed, usage, true);
            out.push(Output::Hotkey(id));
            return;
        }
        self.emit(usage, KeyState::Down, raw.ts_ns, raw.seq, out);
        if repeatable(usage) {
            self.repeater.start(usage, now);
        }
    }

    fn release(&mut self, raw: RawKey, out: &mut Vec<Output>) {
        let usage = raw.usage;
        if !test(&self.down, usage) {
            // Released after a resync, or held across boot: nothing to undo.
            return;
        }
        set(&mut self.down, usage, false);
        self.repeater.release(usage);
        if test(&self.consumed, usage) {
            set(&mut self.consumed, usage, false);
            return;
        }
        self.emit(usage, KeyState::Up, raw.ts_ns, raw.seq, out);
    }

    /// Emit any repeat that is due at tick `now`.
    pub fn tick(&mut self, now: u64, out: &mut Vec<Output>) {
        if let Some(usage) = self.repeater.poll(now) {
            if test(&self.down, usage) {
                self.emit(usage, KeyState::Repeat, now * TICK_NS, self.last_seq, out);
            } else {
                self.repeater.cancel();
            }
        }
    }

    /// The raw stream lost events (`Dropped`): what we think is held may not
    /// be. Release everything (so clients cannot be left with stuck keys) and
    /// forget it; later real releases of those keys are ignored.
    pub fn resync(&mut self, ts_ns: u64, seq: u64, out: &mut Vec<Output>) {
        self.last_seq = seq;
        self.repeater.cancel();
        for usage in self.held() {
            set(&mut self.down, usage, false);
            if test(&self.consumed, usage) {
                set(&mut self.consumed, usage, false);
                continue;
            }
            self.emit(usage, KeyState::Up, ts_ns, seq, out);
        }
    }

    fn emit(&mut self, usage: u16, state: KeyState, ts_ns: u64, seq: u64, out: &mut Vec<Output>) {
        let mods = self.mods();
        let (sym, text) = self.meaning(usage, mods);
        out.push(Output::Key(KeyOut {
            code: usage,
            sym,
            mods,
            state,
            ts_ns,
            seq,
        }));
        let command = mods & (mods::CTRL | mods::ALT | mods::SUPER) != 0;
        if let Some(ch) = text {
            if state != KeyState::Up && !command {
                out.push(Output::Text(String::from(ch)));
            }
        }
    }

    /// The keysym and typed character of `usage` under `mods`.
    fn meaning(&self, usage: u16, mods: u32) -> (u32, Option<char>) {
        let num_lock = mods & mods::NUM_LOCK != 0;
        if let Some((plain, shifted, altgr)) = keymap::levels(self.layout, usage) {
            if mods & mods::ALTGR != 0 {
                // A level the layout leaves empty types nothing (never falls
                // back to the plain character).
                return match altgr {
                    '\0' => (0, None),
                    ch => (ch as u32, Some(ch)),
                };
            }
            let shift = mods & mods::SHIFT != 0;
            let caps = mods & mods::CAPS_LOCK != 0;
            let upper = if plain.is_ascii_lowercase() {
                shift != caps
            } else {
                shift
            };
            let ch = if upper { shifted } else { plain };
            // Shortcuts key on the letter itself, not its case.
            if mods & mods::CTRL != 0 && plain.is_ascii_lowercase() {
                return (plain as u32, Some(ch));
            }
            return (ch as u32, Some(ch));
        }
        match keymap::function_sym(usage, num_lock) {
            Some(sym) => (sym, keymap::keypad_text(usage, num_lock)),
            None => (0, None),
        }
    }
}
