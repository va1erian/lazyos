//! The session rows ("Log out...", "Restart...", "Shut down...") and their
//! in-place confirmation ([`super::power`]).

use super::tests::{pinned, H};
use super::*;

/// The tick a test chooses the first step at, and one past the quiet period.
const T0: u64 = 100;
const LATER: u64 = CONFIRM_QUIET_TICKS;

#[test]
fn the_power_rows_come_last_whatever_is_configured() {
    for configured in [vec![], pinned()] {
        let menu = Menu::build(&[], &configured, Shipped::Unknown, H);
        let labels: Vec<&str> = menu.rows().iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels[labels.len() - 3..],
            ["Log out...", "Restart...", "Shut down..."]
        );
        assert!(menu.rows()[labels.len() - 3..].iter().all(|r| r.enabled));
        assert_eq!(menu.find(""), None, "a power row is never an app");
    }
}

#[test]
fn a_power_row_asks_for_confirmation_in_place() {
    for (power, now) in [
        (Power::Logout, "Log out now"),
        (Power::Reboot, "Restart now"),
        (Power::PowerOff, "Shut down now"),
    ] {
        let mut menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
        let (count, height) = (menu.rows().len(), menu.height());
        let ask = menu.find_action(Action::Ask(power)).unwrap();
        assert!(!menu.confirming());
        assert_eq!(menu.choose(ask, false, T0), Choice::Confirming(power));
        assert!(menu.confirming());
        assert_eq!((menu.rows().len(), menu.height()), (count, height));
        let labels: Vec<&str> = menu.rows()[count - 2..]
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, [now, "Cancel"]);
        // The question above them is a caption: choosing it does nothing.
        assert!(!menu.rows()[count - 3].enabled);
        assert_eq!(menu.choose(count - 3, false, T0), Choice::Nothing);
        assert_eq!(menu.rows()[0].app, "terminal", "the app rows stay");
        // The second press of a double click never confirms.
        assert_eq!(menu.choose(count - 2, true, T0 + LATER), Choice::Nothing);
        // Nor does it cancel through the row under a double-clicked "Shut
        // down...".
        assert_eq!(menu.choose(count - 1, true, T0 + LATER), Choice::Nothing);
        assert!(menu.confirming());
        assert_eq!(menu.choose(count - 2, false, T0 + LATER), Choice::Request(power));
        // Back to the first step, so a refused logout leaves a usable menu.
        assert!(!menu.confirming());
        assert_eq!(menu, Menu::build(&[], &pinned(), Shipped::Unknown, H));
    }
}

#[test]
fn cancel_restores_the_power_rows_and_closes() {
    let mut menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
    let fresh = menu.clone();
    let off = menu.find_action(Action::Ask(Power::PowerOff)).unwrap();
    assert_eq!(menu.choose(off, false, T0), Choice::Confirming(Power::PowerOff));
    let cancel = menu.find_action(Action::Cancel).unwrap();
    assert_eq!(menu.choose(cancel, false, T0 + LATER), Choice::Close);
    assert_eq!(menu, fresh);
}

#[test]
fn a_quick_triple_click_never_requests_the_action() {
    // Press, the double-click backend's repeat, then a plain press again (a
    // pair consumes the sequence): three presses 10 ticks apart.
    for power in [Power::Logout, Power::Reboot, Power::PowerOff] {
        let mut menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
        let fresh = menu.clone();
        let ask = menu.find_action(Action::Ask(power)).unwrap();
        assert_eq!(menu.choose(ask, false, 100), Choice::Confirming(power));
        let confirm = menu.find_action(Action::Confirm(power)).unwrap();
        let cancel = menu.find_action(Action::Cancel).unwrap();
        // The second press repeats, the third is plain (a pair consumes the
        // sequence); neither the confirmation nor "Cancel" under the pointer
        // acts.
        for (at, repeat) in [(110, true), (120, false)] {
            for row in [confirm, cancel] {
                let choice = menu.choose(row, repeat, at);
                assert_eq!(choice, Choice::Nothing, "{power:?} at {at}");
            }
        }
        assert!(menu.confirming(), "{power:?}: still asking");
        // The last quiet tick is still quiet; a deliberate click after it acts.
        assert_eq!(menu.choose(confirm, false, 100 + CONFIRM_QUIET_TICKS - 1), Choice::Nothing);
        assert_eq!(
            menu.choose(confirm, false, 100 + CONFIRM_QUIET_TICKS),
            Choice::Request(power)
        );
        assert_eq!(menu, fresh, "back to the first step, no quiet period left");
    }
}

#[test]
fn a_late_cancel_closes_and_leaves_no_quiet_period() {
    let mut menu = Menu::build(&[], &pinned(), Shipped::Unknown, H);
    let fresh = menu.clone();
    let off = menu.find_action(Action::Ask(Power::PowerOff)).unwrap();
    assert_eq!(menu.choose(off, false, 100), Choice::Confirming(Power::PowerOff));
    let cancel = menu.find_action(Action::Cancel).unwrap();
    assert_eq!(menu.choose(cancel, false, 120), Choice::Nothing, "quiet");
    assert_eq!(menu.choose(cancel, false, 100 + CONFIRM_QUIET_TICKS), Choice::Close);
    assert_eq!(menu, fresh);
}
