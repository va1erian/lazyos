//! Keyboard grabs, the reserved escape chord, the key-state page and the
//! stuck-key guarantees around focus changes (`docs/input-plan.md`, I3).

use super::*;
use crate::grab::{Change, Grabs, Reason, Refused, Requested};
use crate::keystate::{bits_of, SharedKeys, Snapshot, Writer};
use crate::router::Router;
use crate::{mods, KeyState, ESCAPE_CODE};
use alloc::vec::Vec;

const W: u16 = 0x1A;
const ESC: u16 = ESCAPE_CODE;

/// Two windows (surfaces 1 and 2) with a session each; focus on the first.
fn seat() -> (Router, u64, u64) {
    let mut router = Router::new();
    router.register_surface(1, 100).unwrap();
    router.register_surface(2, 200).unwrap();
    let a = router.open(100, 1).unwrap().session;
    let b = router.open(200, 2).unwrap().session;
    router.set_focus(Some(1));
    (router, a, b)
}

fn kinds(out: &[Output]) -> Vec<(u16, KeyState)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Key(k) => Some((k.code, k.state)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_grab_needs_focus_and_the_compositors_yes() {
    let (router, a, b) = seat();
    let mut grabs = Grabs::new();
    let focused = router.focused_session();
    assert_eq!(grabs.request(b, focused), Err(Refused::NotFocused));
    assert_eq!(
        grabs.request(a, focused),
        Ok(Requested::Pending { withdrawn: None })
    );
    assert_eq!(grabs.holder(), None, "a request is not a grab");
    assert_eq!(
        grabs.approve(b, true, focused),
        None,
        "nothing pending for b"
    );
    assert_eq!(
        grabs.approve(a, false, focused),
        Some(Change {
            session: a,
            active: false,
            reason: Reason::Denied,
            holder_changed: false
        })
    );
    assert_eq!(grabs.holder(), None);
    grabs.request(a, focused).unwrap();
    let granted = grabs.approve(a, true, focused).unwrap();
    assert!(granted.active && granted.holder_changed && granted.reason == Reason::Approved);
    assert_eq!(grabs.holder(), Some(a));
    assert_eq!(grabs.request(a, focused), Ok(Requested::AlreadyHeld));
    // Released by its holder; releasing again is a no-op.
    assert_eq!(grabs.release(a).map(|c| c.reason), Some(Reason::Released));
    assert_eq!(grabs.release(a), None);
}

#[test]
fn an_approval_that_comes_after_focus_moved_is_a_denial() {
    let (mut router, a, b) = seat();
    let mut grabs = Grabs::new();
    grabs.request(a, router.focused_session()).unwrap();
    router.set_focus(Some(2));
    // inputd withdraws the request on the focus change...
    let withdrawn = grabs.focus_changed(router.focused_session()).unwrap();
    assert_eq!(
        (withdrawn.session, withdrawn.reason),
        (a, Reason::FocusLost)
    );
    assert!(!withdrawn.holder_changed);
    // ...so the late answer finds nothing.
    assert_eq!(grabs.approve(a, true, router.focused_session()), None);
    // A request the compositor answers once focus has moved on is denied.
    grabs.request(b, router.focused_session()).unwrap();
    router.set_focus(Some(1));
    let late = grabs.approve(b, true, router.focused_session()).unwrap();
    assert_eq!((late.active, late.reason), (false, Reason::Denied));
    assert_eq!(grabs.holder(), None);
}

/// The scenario the plan names: a game grabs the keyboard, the compositor's
/// Alt+Tab goes to the game, and the escape chord always takes it back.
#[test]
fn grab_and_escape_scenario() {
    let (router, a, _) = seat();
    let mut rig = Rig::new(Layout::Us);
    let alt_tab = rig.engine.add_hotkey(TAB, mods::ALT).unwrap();
    let mut grabs = Grabs::new();
    // No grab: Alt+Tab is the compositor's.
    rig.down(LALT);
    assert_eq!(rig.down(TAB), [Output::Hotkey(alt_tab)]);
    rig.up(TAB);
    rig.up(LALT);
    // Granted: Alt+Tab is an ordinary key for the grabber.
    grabs.request(a, router.focused_session()).unwrap();
    grabs.approve(a, true, router.focused_session()).unwrap();
    rig.engine.set_grabbed(grabs.holder().is_some());
    rig.down(LALT);
    assert_eq!(kinds(&rig.down(TAB)), [(TAB, KeyState::Down)]);
    assert_eq!(kinds(&rig.up(TAB)), [(TAB, KeyState::Up)]);
    // Ctrl+Alt+Esc: consumed, never a key event, even while grabbed.
    rig.down(LCTRL);
    assert_eq!(rig.down(ESC), [Output::Escape]);
    assert_eq!(rig.up(ESC), [], "the chord's release is swallowed too");
    let escaped = grabs.escape().unwrap();
    assert_eq!((escaped.session, escaped.active), (a, false));
    assert_eq!(escaped.reason, Reason::Escaped);
    assert!(escaped.holder_changed);
    rig.engine.set_grabbed(grabs.holder().is_some());
    rig.up(LCTRL);
    // The compositor has its chord back.
    assert_eq!(rig.down(TAB), [Output::Hotkey(alt_tab)]);
    rig.up(TAB);
    rig.up(LALT);
    // Escaping with nothing held changes nothing (but is still consumed).
    assert_eq!(grabs.escape(), None);
    rig.down(LCTRL);
    rig.down(LALT);
    assert_eq!(rig.down(ESC), [Output::Escape]);
}

#[test]
fn the_escape_chord_cannot_be_registered_and_plain_escape_is_a_key() {
    let mut rig = Rig::new(Layout::Us);
    assert_eq!(rig.engine.add_hotkey(ESC, mods::CTRL | mods::ALT), None);
    assert_eq!(
        rig.engine
            .add_hotkey(ESC, mods::CTRL | mods::ALT | mods::CAPS_LOCK),
        None,
        "lock bits are not part of a chord"
    );
    assert!(rig.engine.add_hotkey(ESC, mods::CTRL).is_some());
    assert_eq!(kinds(&rig.down(ESC)), [(ESC, KeyState::Down)]);
    rig.up(ESC);
    // Ctrl+Shift+Alt+Esc is not the chord (exact modifiers).
    rig.down(LCTRL);
    rig.down(LALT);
    rig.down(LSHIFT);
    assert_eq!(kinds(&rig.down(ESC)), [(ESC, KeyState::Down)]);
}

#[test]
fn losing_focus_or_closing_ends_a_grab() {
    let (mut router, a, b) = seat();
    let mut grabs = Grabs::new();
    grabs.request(a, router.focused_session()).unwrap();
    grabs.approve(a, true, router.focused_session()).unwrap();
    // Focus staying put changes nothing.
    assert_eq!(grabs.focus_changed(router.focused_session()), None);
    router.set_focus(Some(2));
    let lost = grabs.focus_changed(router.focused_session()).unwrap();
    assert_eq!((lost.session, lost.reason), (a, Reason::FocusLost));
    assert!(lost.holder_changed && !lost.active);
    grabs.request(b, router.focused_session()).unwrap();
    grabs.approve(b, true, router.focused_session()).unwrap();
    assert_eq!(grabs.closed(b).map(|c| c.reason), Some(Reason::Closed));
    assert_eq!(grabs.holder(), None);
}

/// A key held across a focus change is released for the window that lost
/// focus and seeded for the one that gained it; repeat stops; the key-state
/// pages follow (cleared, then live for the new holder).
#[test]
fn stuck_key_on_focus_change() {
    let (mut router, a, b) = seat();
    let mut rig = Rig::new(Layout::Us);
    let (page_a, page_b) = (SharedKeys::new(), SharedKeys::new());
    let (mut writer_a, mut writer_b) = (Writer::new(), Writer::new());
    let publish = |rig: &Rig, focused: Option<u64>, session, writer: &mut Writer, page| {
        writer.publish(
            page,
            Snapshot {
                seq: rig.engine.last_seq(),
                focused: focused == Some(session),
                down: rig.engine.down_bits(),
            },
        )
    };
    rig.down(W);
    publish(&rig, router.focused_session(), a, &mut writer_a, &page_a);
    publish(&rig, router.focused_session(), b, &mut writer_b, &page_b);
    assert!(page_a.snapshot().unwrap().is_down(W));
    assert!(!page_b.snapshot().unwrap().is_down(W), "b is not focused");
    // Held past the delay: it repeats.
    let repeats = rig.advance(crate::REPEAT_DELAY_TICKS + 1);
    assert_eq!(kinds(&repeats), [(W, KeyState::Repeat)]);
    // Focus moves while W is held (inputd's `apply`).
    let change = router.set_focus(Some(2));
    assert_eq!((change.left, change.entered), (Some(a), Some(b)));
    rig.engine.cancel_repeat();
    publish(&rig, router.focused_session(), a, &mut writer_a, &page_a);
    publish(&rig, router.focused_session(), b, &mut writer_b, &page_b);
    let left = page_a.snapshot().unwrap();
    assert!(
        !left.focused && left.down == [0; 4],
        "a's page still shows keys"
    );
    assert!(page_b.snapshot().unwrap().is_down(W));
    assert_eq!(rig.engine.held(), [W], "KeyboardEnter seeds b with W");
    assert_eq!(kinds(&rig.advance(50)), [], "repeat leaked across focus");
    // The release goes to b (the focused one) and clears its page.
    assert_eq!(kinds(&rig.up(W)), [(W, KeyState::Up)]);
    publish(&rig, router.focused_session(), b, &mut writer_b, &page_b);
    assert!(!page_b.snapshot().unwrap().is_down(W));
}

#[test]
fn the_page_is_written_only_on_change_and_reads_consistently() {
    let page = SharedKeys::new();
    let mut writer = Writer::new();
    let state = Snapshot {
        seq: 7,
        focused: true,
        down: bits_of(&[W, 0xE1, 0xFF]),
    };
    assert!(writer.publish(&page, state));
    assert!(!writer.publish(&page, state), "unchanged state rewritten");
    let read = page.snapshot().unwrap();
    assert_eq!(read, state);
    assert!(read.is_down(W) && read.is_down(0xE1) && read.is_down(0xFF));
    assert!(!read.is_down(0x04));
    assert_eq!(page.lock.load(core::sync::atomic::Ordering::Relaxed) % 2, 0);
    // A client scribbling on the lock word stalls only its own reads; the
    // writer never reads it back.
    page.lock.store(1, core::sync::atomic::Ordering::Relaxed);
    assert_eq!(page.snapshot(), None);
    assert!(writer.publish(&page, Snapshot { seq: 8, ..state }));
    assert_eq!(page.snapshot().unwrap().seq, 8);
    // Unfocused, the page says nothing whatever the bits passed in.
    writer.publish(
        &page,
        Snapshot {
            seq: 9,
            focused: false,
            down: bits_of(&[W]),
        },
    );
    let read = page.snapshot().unwrap();
    assert!(!read.focused && read.down == [0; 4] && !read.is_down(W));
    const { assert!(crate::keystate::SIZE <= 4096) };
}

/// Soak: random requests, answers, releases, focus moves, closes and escape
/// chords. The holder and any pending request always have focus, an escape
/// always leaves nothing held, and every change reports the right session.
#[test]
fn soak_grab_invariants() {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let sessions = [1u64, 2, 3, 4];
    let mut grabs = Grabs::new();
    let mut focused: Option<u64> = None;
    let mut held_changes = 0u32;
    for _ in 0..200_000 {
        let session = sessions[(next() % 4) as usize];
        let change = match next() % 7 {
            0 => {
                focused = if next() % 5 == 0 { None } else { Some(session) };
                grabs.focus_changed(focused)
            }
            1 => match grabs.request(session, focused) {
                Ok(Requested::Pending { withdrawn }) => withdrawn,
                Ok(Requested::AlreadyHeld) => None,
                Err(Refused::NotFocused) => {
                    assert_ne!(focused, Some(session));
                    None
                }
            },
            2 | 3 => grabs.approve(session, next() % 3 != 0, focused),
            4 => grabs.release(session),
            5 => grabs.closed(session),
            _ => {
                let change = grabs.escape();
                assert_eq!((grabs.holder(), grabs.pending()), (None, None));
                change
            }
        };
        if let Some(change) = change {
            if change.active {
                assert_eq!(grabs.holder(), Some(change.session));
                held_changes += 1;
            } else {
                assert_ne!(grabs.holder(), Some(change.session));
            }
        }
        for who in [grabs.holder(), grabs.pending()].into_iter().flatten() {
            assert_eq!(Some(who), focused, "a grab or request without focus");
        }
    }
    assert!(held_changes > 100, "only {held_changes} grants in the soak");
}
