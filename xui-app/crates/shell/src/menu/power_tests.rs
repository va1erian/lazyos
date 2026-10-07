//! The session rows ("Log out...", "Restart...", "Shut down...") and their
//! in-place confirmation ([`super::power`]).

use super::tests::{pinned, H};
use super::*;

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
        assert_eq!(menu.choose(ask, false), Choice::Confirming(power));
        assert!(menu.confirming());
        assert_eq!((menu.rows().len(), menu.height()), (count, height));
        let labels: Vec<&str> = menu.rows()[count - 2..]
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, [now, "Cancel"]);
        // The question above them is a caption: choosing it does nothing.
        assert!(!menu.rows()[count - 3].enabled);
        assert_eq!(menu.choose(count - 3, false), Choice::Nothing);
        assert_eq!(menu.rows()[0].app, "terminal", "the app rows stay");
        // The second press of a double click never confirms.
        assert_eq!(menu.choose(count - 2, true), Choice::Nothing);
        assert_eq!(menu.choose(count - 2, false), Choice::Request(power));
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
    assert_eq!(menu.choose(off, false), Choice::Confirming(Power::PowerOff));
    let cancel = menu.find_action(Action::Cancel).unwrap();
    assert_eq!(menu.choose(cancel, false), Choice::Close);
    assert_eq!(menu, fresh);
}
