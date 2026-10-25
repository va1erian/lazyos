//! Dialogs of the tabs (docs/lazyweb-plan.md 2.2): a tab behind the front one that asks something
//! comes to the front when the person is free for it, never in the middle of another dialog or of
//! typing an address, and never in the middle of a switch.

mod support;

use support::*;
use xui_core::Key;
use xui_lazyweb::app::Msg;
use xui_webkit::engine::{Call, DialogKind};
use xui_webkit::events::Event;

fn visibility(calls: &[Call]) -> Vec<bool> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::SetVisible(on) => Some(*on),
            _ => None,
        })
        .collect()
}

fn alert(id: u64, text: &str) -> Event {
    Event::Dialog {
        id,
        kind: DialogKind::Alert,
        text: text.into(),
        default: String::new(),
    }
}

#[test]
fn a_dialog_from_a_tab_behind_brings_it_to_the_front_and_the_answer_goes_to_it() {
    drive("tab-dialog", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        // The new tab's address bar is being typed in: the person is done with it first.
        stage.emit(Msg::RestoreAddress);
        rig.engines.events(
            stage,
            0,
            vec![Event::Dialog {
                id: 5,
                kind: DialogKind::Confirm,
                text: "Leave?".into(),
                default: String::new(),
            }],
            false,
        );
        assert_eq!(
            rig.engines.fronts.borrow().last(),
            Some(&0),
            "the asking tab is in front"
        );
        stage.inject(key_down(Key::ESCAPE));
        assert!(rig
            .engines
            .calls(0)
            .contains(&Call::DialogReply(5, false, String::new())));
        assert!(!rig
            .engines
            .calls(1)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(..))));
    });
}

#[test]
fn a_dialog_waits_while_the_front_tab_has_one_open_and_comes_when_it_is_answered() {
    drive("tab-dialog-ids", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        stage.emit(Msg::RestoreAddress);
        for (tab, front) in [(1, true), (0, false)] {
            rig.engines.events(stage, tab, vec![alert(1, "Hi")], front);
        }
        // Same number in two tabs. The front tab's dialog stays up: the other tab's waits, and
        // nothing was said to either page.
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&1));
        assert!(!rig
            .engines
            .calls(1)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(..))));
        stage.inject(key_down(Key::ESCAPE));
        assert!(
            rig.engines
                .calls(1)
                .contains(&Call::DialogReply(1, false, String::new())),
            "the front tab's own"
        );
        // Now the waiting one comes, and its answer goes to the tab that asked.
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&0));
        assert!(!rig
            .engines
            .calls(0)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(..))));
        stage.inject(key_down(Key::ESCAPE));
        assert!(rig
            .engines
            .calls(0)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(1, ..))));
    });
}

#[test]
fn a_dialog_from_a_tab_behind_does_not_take_the_front_while_an_address_is_typed() {
    drive("tab-dialog-typing", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        // Ctrl+T put the cursor in the address bar of the new tab.
        rig.engines.events(stage, 0, vec![alert(2, "Hi")], false);
        assert_eq!(
            rig.engines.fronts.borrow().last(),
            Some(&1),
            "the person is typing: the tab stays where it is"
        );
        // The strip marks the tab that waits; the tab closing takes its dialog with it.
        assert!(!rig
            .engines
            .calls(0)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(..))));
        stage.emit(Msg::RestoreAddress);
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&0));
        stage.inject(key_down(Key::ESCAPE));
        assert!(rig
            .engines
            .calls(0)
            .contains(&Call::DialogReply(2, false, String::new())));
    });
}

#[test]
fn a_dialog_waiting_in_a_tab_that_closes_is_forgotten() {
    drive("tab-dialog-closed", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        rig.engines.events(stage, 0, vec![alert(2, "Hi")], false);
        // Close tab 0 (its strip button) while its dialog waits: tab 1 stays in front, and no
        // dialog comes for a page that is gone.
        let (x, y) = rig.centre("tab_1_close");
        stage.click(x, y);
        assert_eq!(*rig.engines.gone.borrow(), [0]);
        stage.emit(Msg::RestoreAddress);
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&1));
        stage.inject(key_down(Key::ESCAPE));
        assert!(!rig
            .engines
            .calls(0)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(..))));
    });
}

#[test]
fn a_dialog_the_leaving_tab_had_pending_does_not_take_the_switch_apart() {
    drive("tab-dialog-leaving", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        stage.emit(Msg::RestoreAddress);
        // Tab 1 is in front and has asked something the window has not read yet (the notice is
        // still on its way); the person presses Ctrl+Tab before it arrives.
        rig.engines.made.borrow()[1]
            .mailbox
            .post_event(alert(3, "Wait"));
        stage.inject(chord(Key::TAB, false));
        // The switch was made whole (tab 0 in front, tab 1 behind) and then the dialog of tab 1
        // brought it back: the window and the model agree on tab 1 and nothing is half done.
        let fronts = rig.engines.fronts.borrow().clone();
        assert_eq!(fronts.last(), Some(&1), "{fronts:?}");
        let made = rig.engines.made.borrow();
        let seen = visibility(&made[0].engine.calls());
        assert!(seen.windows(2).all(|pair| pair[0] != pair[1]), "{seen:?}");
        let seen = visibility(&made[1].engine.calls());
        assert!(seen.windows(2).all(|pair| pair[0] != pair[1]), "{seen:?}");
        assert_eq!(seen.last(), Some(&true), "the tab with the dialog is shown");
        drop(made);
        stage.inject(key_down(Key::ESCAPE));
        assert!(rig
            .engines
            .calls(1)
            .contains(&Call::DialogReply(3, false, String::new())));
    });
}
