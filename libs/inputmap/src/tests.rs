//! Host tests for the keymaps and the engine.
//!
//! The port-fidelity tests cross-check the compiled-in layouts against the
//! kernel's original scancode-keyed `layout.rs` data, translating through the
//! kernel's own set-1 to HID table (`#[path]`-included, so the two cannot
//! drift apart unnoticed).

use alloc::string::String;
use alloc::vec::Vec;

use crate::keymap::{self, Layout};
use crate::keysym;
use crate::mods;
use crate::{Engine, KeyOut, KeyState, Output, RawKey, REPEAT_DELAY_TICKS, REPEAT_INTERVAL_TICKS};

#[path = "../../../kernel/src/input/hid.rs"]
#[allow(dead_code)]
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

#[test]
fn us_letters_digits_and_symbols() {
    let mut rig = Rig::new(Layout::Us);
    assert_eq!(typed(&mut rig, A).as_deref(), Some("a"));
    assert_eq!(typed(&mut rig, Q).as_deref(), Some("q"));
    assert_eq!(typed(&mut rig, ONE).as_deref(), Some("1"));
    assert_eq!(typed(&mut rig, SPACE).as_deref(), Some(" "));
    rig.down(LSHIFT);
    assert_eq!(typed(&mut rig, A).as_deref(), Some("A"));
    assert_eq!(typed(&mut rig, ONE).as_deref(), Some("!"));
    assert_eq!(typed(&mut rig, 0x33).as_deref(), Some(":"));
    rig.up(LSHIFT);
    // AltGr is a French concept: right Alt is Alt on US and types nothing.
    rig.down(RALT);
    assert_eq!(typed(&mut rig, 0x27), None);
}

#[test]
fn french_matches_the_kernels_original_tables() {
    // The kernel's scancode-keyed AZERTY data, verbatim.
    fn letter_at(code: u8) -> Option<char> {
        const ROWS: [(u8, &str); 3] =
            [(0x10, "azertyuiop"), (0x1E, "qsdfghjklm"), (0x2C, "wxcvbn")];
        ROWS.iter().find_map(|&(first, letters)| {
            let index = code.checked_sub(first)? as usize;
            letters.chars().nth(index)
        })
    }
    fn symbol_at(code: u8) -> Option<(char, char, char)> {
        Some(match code {
            0x02 => ('&', '1', '\0'),
            0x03 => ('é', '2', '~'),
            0x04 => ('"', '3', '#'),
            0x05 => ('\'', '4', '{'),
            0x06 => ('(', '5', '['),
            0x07 => ('-', '6', '|'),
            0x08 => ('è', '7', '`'),
            0x09 => ('_', '8', '\\'),
            0x0A => ('ç', '9', '^'),
            0x0B => ('à', '0', '@'),
            0x0C => (')', '°', ']'),
            0x0D => ('=', '+', '}'),
            0x1A => ('^', '¨', '\0'),
            0x1B => ('$', '£', '\0'),
            0x28 => ('ù', '%', '\0'),
            0x29 => ('²', '³', '\0'),
            0x2B => ('*', 'µ', '\0'),
            0x32 => (',', '?', '\0'),
            0x33 => (';', '.', '\0'),
            0x34 => (':', '/', '\0'),
            0x35 => ('!', '§', '\0'),
            0x56 => ('<', '>', '\0'),
            _ => return None,
        })
    }
    let mut compared = 0;
    for code in 0x01..0x59u8 {
        let Some(usage) = kernel_hid::translate(false, code) else {
            continue;
        };
        let new = keymap::levels(Layout::Fr, usage);
        let old = match (letter_at(code), symbol_at(code)) {
            (Some(letter), _) => Some((letter, letter.to_ascii_uppercase(), '\0')),
            // Space is not layout-specific: the kernel's shared table typed it.
            (None, _) if code == 0x39 => Some((' ', ' ', '\0')),
            (None, sym) => sym,
        };
        assert_eq!(new, old, "scancode {code:#x} usage {usage:#x}");
        compared += (old.is_some()) as usize;
    }
    // 26 letters, the digit row, and every symbol key.
    assert!(compared >= 46, "only {compared} keys compared");
}

#[test]
fn us_matches_the_kernels_original_table() {
    // The kernel's US decode, as (scancode, plain, shifted).
    let table: &[(u8, char, char)] = &[
        (0x02, '1', '!'),
        (0x03, '2', '@'),
        (0x04, '3', '#'),
        (0x05, '4', '$'),
        (0x06, '5', '%'),
        (0x07, '6', '^'),
        (0x08, '7', '&'),
        (0x09, '8', '*'),
        (0x0A, '9', '('),
        (0x0B, '0', ')'),
        (0x0C, '-', '_'),
        (0x0D, '=', '+'),
        (0x1A, '[', '{'),
        (0x1B, ']', '}'),
        (0x27, ';', ':'),
        (0x28, '\'', '"'),
        (0x29, '`', '~'),
        (0x2B, '\\', '|'),
        (0x33, ',', '<'),
        (0x34, '.', '>'),
        (0x35, '/', '?'),
        (0x39, ' ', ' '),
    ];
    for &(code, plain, shifted) in table {
        let usage = kernel_hid::translate(false, code).unwrap();
        assert_eq!(
            keymap::levels(Layout::Us, usage),
            Some((plain, shifted, '\0')),
            "scancode {code:#x}"
        );
    }
    // Letters: QWERTY rows of set-1 scancodes.
    for (first, letters) in [
        (0x10u8, "qwertyuiop"),
        (0x1E, "asdfghjkl"),
        (0x2C, "zxcvbnm"),
    ] {
        for (index, letter) in letters.chars().enumerate() {
            let usage = kernel_hid::translate(false, first + index as u8).unwrap();
            assert_eq!(
                keymap::levels(Layout::Us, usage),
                Some((letter, letter.to_ascii_uppercase(), '\0'))
            );
        }
    }
    // ANSI boards have no ISO key.
    assert_eq!(keymap::levels(Layout::Us, 0x64), None);
}

#[test]
fn french_altgr_layer_and_empty_levels() {
    let mut rig = Rig::new(Layout::Fr);
    rig.down(RALT);
    assert_eq!(rig.engine.mods() & mods::ALTGR, mods::ALTGR);
    assert_eq!(
        rig.engine.mods() & mods::ALT,
        0,
        "AltGr is not Alt on AZERTY"
    );
    assert_eq!(typed(&mut rig, 0x27).as_deref(), Some("@"));
    assert_eq!(typed(&mut rig, 0x21).as_deref(), Some("{"));
    assert_eq!(typed(&mut rig, 0x2D).as_deref(), Some("]"));
    // No AltGr entry: nothing, not the plain `^`.
    let out = rig.down(0x2F);
    assert_eq!(key(&out).sym, 0);
    assert_eq!(text(&out), None);
    rig.up(0x2F);
    rig.up(RALT);
    assert_eq!(typed(&mut rig, 0x27).as_deref(), Some("à"));
}

#[test]
fn key_events_carry_physical_code_sym_and_post_event_mods() {
    let mut rig = Rig::new(Layout::Fr);
    let out = rig.down(LSHIFT);
    let shift = key(&out);
    assert_eq!(shift.code, LSHIFT);
    assert_eq!(shift.sym, keysym::SHIFT_L);
    assert_eq!(shift.state, KeyState::Down);
    assert_eq!(shift.mods & mods::SHIFT, mods::SHIFT);
    assert_eq!(text(&out), None, "modifiers type nothing");
    // The physical key at QWERTY-Q is `a` on AZERTY; the code stays Q's.
    let out = rig.down(Q);
    let a = key(&out);
    assert_eq!((a.code, a.sym), (Q, 'A' as u32));
    assert!(a.ts_ns > 0 && a.seq == 2);
    rig.up(Q);
    let out = rig.up(LSHIFT);
    assert_eq!(key(&out).mods & mods::SHIFT, 0, "release drops the bit");
    assert_eq!(key(&out).state, KeyState::Up);
    // Non-character keys have function keysyms and no text.
    let out = rig.down(ENTER);
    assert_eq!((key(&out).sym, text(&out)), (keysym::ENTER, None));
    assert_eq!(key(&rig.down(F4)).sym, keysym::F1 + 3);
}

#[test]
fn caps_lock_toggles_letters_only() {
    let mut rig = Rig::new(Layout::Us);
    rig.down(CAPS);
    rig.up(CAPS);
    assert_eq!(rig.engine.mods() & mods::CAPS_LOCK, mods::CAPS_LOCK);
    assert_eq!(typed(&mut rig, A).as_deref(), Some("A"));
    assert_eq!(
        typed(&mut rig, ONE).as_deref(),
        Some("1"),
        "digits unaffected"
    );
    rig.down(LSHIFT);
    assert_eq!(
        typed(&mut rig, A).as_deref(),
        Some("a"),
        "Shift inverts Caps"
    );
    assert_eq!(typed(&mut rig, ONE).as_deref(), Some("!"));
    rig.up(LSHIFT);
    rig.down(CAPS);
    rig.up(CAPS);
    assert_eq!(typed(&mut rig, A).as_deref(), Some("a"));
}

#[test]
fn keypad_follows_num_lock() {
    let mut rig = Rig::new(Layout::Us);
    let out = rig.down(KP_1);
    assert_eq!(
        (key(&out).sym, text(&out).as_deref()),
        (keysym::KP_0 + 1, Some("1"))
    );
    rig.up(KP_1);
    rig.down(NUM);
    rig.up(NUM);
    let out = rig.down(KP_1);
    assert_eq!((key(&out).sym, text(&out)), (keysym::END, None));
    rig.up(KP_1);
    let out = rig.down(KP_0);
    assert_eq!(key(&out).sym, keysym::INSERT);
    // Operators type regardless.
    assert_eq!(typed(&mut rig, 0x57).as_deref(), Some("+"));
}

#[test]
fn command_chords_type_no_text() {
    let mut rig = Rig::new(Layout::Us);
    rig.down(LCTRL);
    let out = rig.down(A);
    assert_eq!((key(&out).sym, text(&out)), ('a' as u32, None));
    // Ctrl+Shift+A still reports the unshifted letter; Shift is in `mods`.
    rig.up(A);
    rig.down(LSHIFT);
    let out = rig.down(A);
    assert_eq!(key(&out).sym, 'a' as u32);
    assert_eq!(key(&out).mods & mods::CHORD_MASK, mods::CTRL | mods::SHIFT);
    rig.up(A);
    rig.up(LSHIFT);
    rig.up(LCTRL);
    rig.down(LALT);
    assert_eq!(typed(&mut rig, A), None, "Alt chords type nothing");
    rig.up(LALT);
    rig.down(LGUI);
    assert_eq!(typed(&mut rig, A), None, "Super chords type nothing");
}

#[test]
fn repeat_starts_after_the_delay_and_is_flagged() {
    let mut rig = Rig::new(Layout::Us);
    rig.down(A);
    assert!(rig.advance(REPEAT_DELAY_TICKS - 1).is_empty(), "too early");
    let out = rig.advance(1);
    let repeat = key(&out);
    assert_eq!((repeat.code, repeat.state), (A, KeyState::Repeat));
    assert_eq!(
        text(&out).as_deref(),
        Some("a"),
        "repeat retypes the character"
    );
    assert!(rig.advance(REPEAT_INTERVAL_TICKS - 1).is_empty());
    assert_eq!(key(&rig.advance(1)).state, KeyState::Repeat);
    // Releasing stops it, and the release itself is a plain Up.
    assert_eq!(key(&rig.up(A)).state, KeyState::Up);
    assert!(rig.advance(200).is_empty());
}

#[test]
fn repeat_follows_the_newest_key_and_skips_modifiers() {
    let mut rig = Rig::new(Layout::Us);
    rig.down(LSHIFT);
    assert!(rig.advance(500).is_empty(), "modifiers never repeat");
    rig.down(A);
    rig.advance(10);
    rig.down(0x05); // B
    assert!(
        rig.advance(REPEAT_DELAY_TICKS - 1).is_empty(),
        "delay restarts"
    );
    let out = rig.advance(1);
    assert_eq!(key(&out).code, 0x05);
    // Releasing the *other* key leaves the repeat alone.
    rig.up(A);
    assert_eq!(key(&rig.advance(REPEAT_INTERVAL_TICKS)).code, 0x05);
    // A late poll produces one repeat, not a catch-up burst.
    let out = rig.advance(100);
    assert_eq!(
        out.iter().filter(|o| matches!(o, Output::Key(_))).count(),
        1
    );
}

#[test]
fn repeat_cancels_on_focus_change_and_layout_change() {
    let mut rig = Rig::new(Layout::Us);
    rig.down(A);
    rig.engine.cancel_repeat();
    assert!(rig.advance(500).is_empty());
    rig.up(A);
    rig.down(A);
    rig.engine.set_layout(Layout::Fr);
    assert!(rig.advance(500).is_empty());
    assert_eq!(rig.engine.next_due(), None);
}

#[test]
fn hotkeys_consume_the_press_and_its_release() {
    let mut rig = Rig::new(Layout::Us);
    let alt_tab = rig.engine.add_hotkey(TAB, mods::ALT);
    let super_alone = rig.engine.add_hotkey(LGUI, 0);
    // Alt is delivered as a key; Tab is consumed.
    assert_eq!(key(&rig.down(LALT)).code, LALT);
    assert_eq!(rig.down(TAB), [Output::Hotkey(alt_tab)]);
    assert!(rig.advance(500).is_empty(), "a consumed key never repeats");
    assert!(rig.up(TAB).is_empty(), "release swallowed");
    // Extra modifiers do not match an exact chord.
    rig.down(LSHIFT);
    assert_eq!(key(&rig.down(TAB)).code, TAB);
    rig.up(TAB);
    rig.up(LSHIFT);
    rig.up(LALT);
    // Without Alt, Tab is an ordinary key.
    assert_eq!(key(&rig.down(TAB)).code, TAB);
    rig.up(TAB);
    // A modifier-only hotkey fires on the modifier itself.
    assert_eq!(rig.down(LGUI), [Output::Hotkey(super_alone)]);
    rig.up(LGUI);
    assert!(rig.engine.remove_hotkey(alt_tab) && !rig.engine.remove_hotkey(alt_tab));
    rig.down(LALT);
    assert_eq!(
        key(&rig.down(TAB)).code,
        TAB,
        "unregistered chord passes through"
    );
}

#[test]
fn resync_releases_everything_and_ignores_late_releases() {
    let mut rig = Rig::new(Layout::Us);
    rig.down(CAPS);
    rig.up(CAPS);
    rig.down(LSHIFT);
    rig.down(A);
    assert_eq!(rig.engine.held(), [A, LSHIFT]);
    let mut out = Vec::new();
    rig.engine.resync(999, 50, &mut out);
    let ups: Vec<(u16, KeyState)> = out
        .iter()
        .filter_map(|o| match o {
            Output::Key(k) => Some((k.code, k.state)),
            _ => None,
        })
        .collect();
    assert_eq!(ups, [(A, KeyState::Up), (LSHIFT, KeyState::Up)]);
    assert!(rig.engine.held().is_empty());
    assert!(rig.advance(500).is_empty(), "repeat cancelled");
    // The real releases arrive later and change nothing.
    assert!(rig.up(A).is_empty() && rig.up(LSHIFT).is_empty());
    // Locks are state, not held keys: they survive.
    assert_eq!(rig.engine.mods() & mods::CAPS_LOCK, mods::CAPS_LOCK);
}

#[test]
fn duplicate_press_is_not_a_new_press() {
    let mut rig = Rig::new(Layout::Us);
    assert_eq!(rig.down(A).len(), 2);
    assert!(rig.down(A).is_empty());
    assert_eq!(key(&rig.up(A)).state, KeyState::Up);
}

/// A long pseudo-random edge stream: every Up follows a Down of the same key,
/// text only accompanies Down/Repeat, and nothing panics.
#[test]
fn engine_soak_keeps_its_invariants() {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    for layout in [Layout::Us, Layout::Fr] {
        let mut rig = Rig::new(layout);
        let mut down = [false; 256];
        let keys: Vec<u16> = (0x04..=0x65).chain(0xE0..=0xE7).collect();
        for step in 0..400_000u64 {
            let usage = keys[(next() % keys.len() as u64) as usize];
            let mut resynced = false;
            let out = match next() % 5 {
                0 => rig.advance(1 + next() % 60),
                1 if step % 997 == 0 => {
                    let mut out = Vec::new();
                    rig.engine.resync(0, step, &mut out);
                    resynced = true;
                    out
                }
                _ if down[usage as usize] => rig.up(usage),
                _ => rig.down(usage),
            };
            for output in &out {
                match output {
                    Output::Key(k) => match k.state {
                        KeyState::Down => down[k.code as usize] = true,
                        KeyState::Up => assert!(
                            std::mem::replace(&mut down[k.code as usize], false),
                            "Up without Down for {:#x}",
                            k.code
                        ),
                        KeyState::Repeat => {
                            assert!(down[k.code as usize], "repeat of a released key")
                        }
                    },
                    Output::Text(t) => {
                        assert_eq!(t.chars().count(), 1);
                        assert!(t.chars().next().unwrap() as u32 <= 0xFF, "outside Latin-1");
                    }
                    Output::Hotkey(_) => unreachable!("none registered"),
                }
            }
            if resynced {
                down = [false; 256];
            }
            // Keys the engine thinks are held are those the harness saw go down
            // (consumed hotkeys aside, and there are none here).
            if step % 5000 == 0 {
                let held: Vec<u16> = (0..256u16).filter(|&u| down[u as usize]).collect();
                assert_eq!(rig.engine.held(), held);
            }
        }
    }
}

#[test]
fn layout_names_round_trip() {
    for layout in [Layout::Us, Layout::Fr] {
        assert_eq!(Layout::from_name(layout.name()), Some(layout));
    }
    assert_eq!(Layout::from_name("FR"), Some(Layout::Fr));
    assert_eq!(Layout::from_name("dvorak"), None);
    assert_eq!(Layout::from_name(""), None);
}
