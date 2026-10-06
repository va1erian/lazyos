//! Engine behaviour: events, modifiers, locks, repeat, hotkeys, resync.

use super::*;
use crate::keymap::Layout;
use crate::keysym;
use crate::mods;
use crate::{KeyState, ESCAPE_CODE, REPEAT_DELAY_TICKS, REPEAT_INTERVAL_TICKS};
use alloc::vec::Vec;

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
    let alt_tab = rig.engine.add_hotkey(TAB, mods::ALT).unwrap();
    let super_alone = rig.engine.add_hotkey(LGUI, 0).unwrap();
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
        let mut escape_held = false;
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
                    // Ctrl+Alt+Esc: consumed, but held for the engine.
                    Output::Escape => escape_held = true,
                }
            }
            if resynced || (usage == ESCAPE_CODE && !rig.engine.held().contains(&usage)) {
                escape_held = false;
            }
            if resynced {
                down = [false; 256];
            }
            // Keys the engine thinks are held are those the harness saw go down
            // (consumed hotkeys aside, and there are none here).
            if step % 5000 == 0 {
                let held: Vec<u16> = (0..256u16)
                    .filter(|&u| down[u as usize] || (escape_held && u == ESCAPE_CODE))
                    .collect();
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
