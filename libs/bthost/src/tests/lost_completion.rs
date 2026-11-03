//! A Command Complete the controller never sends: the host gives up within
//! the command timeout, says so once, and goes inert.

use bthci::flow::COMMAND_TIMEOUT_MS;

use super::*;

#[test]
fn a_dropped_command_complete_fails_the_host_within_the_timeout() {
    let mut host = Host::new(Config::default(), BondStore::new(), 0);
    // The first command (HCI_Reset) goes out and is never answered.
    assert!(host.poll_command(0).is_some());
    let deadline = host.next_deadline().expect("the command has a deadline");
    assert!(deadline <= COMMAND_TIMEOUT_MS, "{deadline}");
    assert!(!host.is_failed());
    host.on_tick(COMMAND_TIMEOUT_MS - 1);
    assert!(host.poll_effect().is_none(), "not yet");

    host.on_tick(COMMAND_TIMEOUT_MS);
    let mut failures = 0;
    while let Some(effect) = host.poll_effect() {
        failures += usize::from(matches!(effect, Effect::Failed(_)));
    }
    assert_eq!(failures, 1, "Effect::Failed, once");
    assert!(host.is_failed());

    // Inert: no deadline, no packets, late input is ignored, no second Failed.
    assert_eq!(host.next_deadline(), None);
    assert!(host.poll_command(COMMAND_TIMEOUT_MS).is_none());
    assert!(host.poll_acl().is_none());
    host.on_event(
        COMMAND_TIMEOUT_MS + 1,
        &[0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00],
    );
    host.on_tick(COMMAND_TIMEOUT_MS * 3);
    assert!(host.poll_effect().is_none());
    assert!(host.poll_command(COMMAND_TIMEOUT_MS * 3).is_none());
}
