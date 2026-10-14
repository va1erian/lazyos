//! `volumes`: which disks get a daemon, who it belongs to, and how a daemon
//! is stopped (logout, eject, an unresponsive stick).

use crate::volumes::*;
use alloc::vec::Vec;

fn disk(id: u32, generation: u64) -> DiskView {
    DiskView {
        id,
        generation,
        writable: true,
        claimed: false,
    }
}

const USER: (u32, u32) = (1000, 1000);

#[test]
fn a_new_disk_starts_one_daemon_for_the_session_user() {
    let mut v = Volumes::new();
    let plan = v.plan(0, &[disk(0, 1)], Some(USER));
    assert_eq!(
        plan,
        [Action::Start {
            disk: 0,
            generation: 1,
            owner: USER
        }]
    );
    // Asking again before the caller reports changes nothing.
    assert!(v.plan(1, &[disk(0, 1)], Some(USER)).is_empty());
    v.started(0, 1, 77);
    assert!(v.plan(2, &[disk(0, 1)], Some(USER)).is_empty());
    assert_eq!(v.entries()[0].state, State::Running);
}

#[test]
fn nothing_mounts_without_a_session_or_on_a_claimed_disk() {
    let mut v = Volumes::new();
    assert!(v.plan(0, &[disk(0, 1)], None).is_empty());
    let claimed = DiskView {
        claimed: true,
        ..disk(1, 1)
    };
    assert!(v.plan(0, &[claimed], Some(USER)).is_empty());
    // The session arrives: the disk that was waiting starts.
    assert_eq!(v.plan(0, &[disk(0, 1)], Some(USER)).len(), 1);
}

#[test]
fn an_exit_is_the_answer_for_that_plug_until_it_is_replugged() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(2, 5)], Some(USER));
    v.started(2, 5, 9);
    let ended = v.exited(9, code::NO_VOLUME).unwrap();
    assert_eq!(ended.state, State::Ended(Outcome::NoVolume));
    // Not tried again while it is the same plug.
    assert!(v.plan(1, &[disk(2, 5)], Some(USER)).is_empty());
    // Unplugged: forgotten. Plugged again (a new generation): tried.
    assert!(v.plan(2, &[], Some(USER)).is_empty());
    assert!(v.entries().is_empty());
    assert_eq!(v.plan(3, &[disk(2, 6)], Some(USER)).len(), 1);
}

#[test]
fn a_slot_given_to_another_disk_ends_the_old_daemon_at_once() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    v.started(0, 1, 40);
    // The old medium is gone: there is nothing left to flush, so no grace.
    let plan = v.plan(5, &[disk(0, 2)], Some(USER));
    assert_eq!(
        plan,
        [
            Action::Kill { pid: 40 },
            Action::Start {
                disk: 0,
                generation: 2,
                owner: USER
            }
        ]
    );
}

#[test]
fn logging_out_asks_every_daemon_to_finish_and_waits_for_them() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1), disk(1, 1)], Some(USER));
    v.started(0, 1, 10);
    v.started(1, 1, 11);
    let stops = v.plan(100, &[disk(0, 1), disk(1, 1)], None);
    assert_eq!(stops, [Action::Stop { pid: 10 }, Action::Stop { pid: 11 }]);
    // Asked once; the entries stay until the daemons have exited, so the
    // disks are not handed to the next user while they are still flushing.
    assert_eq!(v.entries().len(), 2);
    assert!(v.plan(150, &[disk(0, 1), disk(1, 1)], None).is_empty());
    let other = (1001, 1001);
    assert!(v
        .plan(160, &[disk(0, 1), disk(1, 1)], Some(other))
        .iter()
        .all(|a| !matches!(a, Action::Start { .. })));
    // One finishes; the next look forgets it and the next user gets that
    // disk, not the one still flushing.
    v.exited(10, 0).unwrap();
    let plan = v.plan(170, &[disk(0, 1), disk(1, 1)], Some(other));
    assert!(plan.iter().any(|a| matches!(
        a,
        Action::Start { disk: 0, owner, .. } if *owner == other
    )));
    assert!(!plan
        .iter()
        .any(|a| matches!(a, Action::Start { disk: 1, .. })));
    v.exited(11, 0).unwrap();
    let plan = v.plan(180, &[disk(0, 1), disk(1, 1)], Some(other));
    assert!(plan
        .iter()
        .any(|a| matches!(a, Action::Start { disk: 1, .. })));
}

#[test]
fn a_daemon_that_ignores_the_stop_is_killed_after_the_grace() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    v.started(0, 1, 10);
    assert_eq!(
        v.plan(1000, &[disk(0, 1)], None),
        [Action::Stop { pid: 10 }]
    );
    assert!(v
        .plan(1000 + LEAVE_TICKS - 1, &[disk(0, 1)], None)
        .is_empty());
    assert_eq!(
        v.plan(1000 + LEAVE_TICKS, &[disk(0, 1)], None),
        [Action::Kill { pid: 10 }]
    );
    // The entry is final: never killed twice, forgotten at the next look.
    assert!(v
        .plan(1000 + LEAVE_TICKS + 1, &[disk(0, 1)], None)
        .is_empty());
    assert!(v.entries().is_empty());
}

#[test]
fn a_stick_pulled_while_stopping_is_killed_not_waited_for() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    v.started(0, 1, 10);
    v.plan(10, &[disk(0, 1)], None);
    assert_eq!(v.plan(20, &[], None), [Action::Kill { pid: 10 }]);
}

#[test]
fn an_eject_is_owned_by_the_user_and_ends_in_ejected() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(3, 7)], Some(USER));
    v.started(3, 7, 50);
    assert_eq!(v.eject_target(9, 1000), Err(EjectError::Unknown));
    assert_eq!(v.eject_target(3, 1001), Err(EjectError::NotYours));
    assert_eq!(v.eject_target(3, 1000), Ok((7, Some(50))));
    // Root may eject anyone's.
    assert_eq!(v.eject_target(3, 0), Ok((7, Some(50))));

    v.ejecting(3, 7, 200);
    assert_eq!(v.eject_target(3, 1000), Err(EjectError::Ejecting));
    // Under way: nothing to do, the daemon is not touched.
    assert!(v.plan(300, &[disk(3, 7)], Some(USER)).is_empty());
    let ended = v.exited(50, 0).unwrap();
    assert_eq!(ended.state, State::Ended(Outcome::Ejected));
    assert_eq!(v.eject_target(3, 1000), Err(EjectError::Ejected));
    // The stick is still in: it is not mounted again.
    assert!(v.plan(400, &[disk(3, 7)], Some(USER)).is_empty());
    // A clean exit nobody asked for is not an eject.
    v.plan(0, &[disk(4, 1)], Some(USER));
    v.started(4, 1, 51);
    let ended = v.exited(51, 0).unwrap();
    assert_eq!(ended.state, State::Ended(Outcome::Stopped));
    assert_eq!(v.eject_target(4, 1000), Err(EjectError::Ended));
}

#[test]
fn an_eject_that_never_finishes_is_killed_and_reported() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    v.started(0, 1, 10);
    v.ejecting(0, 1, 100);
    assert!(v
        .plan(100 + EJECT_TICKS - 1, &[disk(0, 1)], Some(USER))
        .is_empty());
    assert_eq!(
        v.plan(100 + EJECT_TICKS, &[disk(0, 1)], Some(USER)),
        [Action::Kill { pid: 10 }]
    );
    assert!(matches!(
        &v.entries()[0].state,
        State::Ended(Outcome::Failed(why)) if why.contains("stopped answering")
    ));
}

#[test]
fn a_volume_that_never_mounted_is_ejected_without_its_daemon() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    v.started(0, 1, 10);
    v.ejected_unmounted(0, 1);
    assert_eq!(v.entries()[0].state, State::Ended(Outcome::Ejected));
    assert_eq!(v.entries()[0].pid, None);
    // The daemon's own exit, whenever it comes, matches nothing.
    assert!(v.exited(10, 137).is_none());
}

#[test]
fn another_users_volume_is_stopped_when_the_session_changes_even_mid_eject() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    v.started(0, 1, 10);
    v.ejecting(0, 1, 5);
    assert_eq!(
        v.plan(6, &[disk(0, 1)], Some((1001, 1001))),
        [Action::Stop { pid: 10 }]
    );
    // Its exit is a stop, not an eject: the owner has gone.
    assert_eq!(
        v.exited(10, 0).unwrap().state,
        State::Ended(Outcome::Stopped)
    );
}

#[test]
fn exit_codes_read_as_words() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    v.started(0, 1, 3);
    let ended = v.exited(3, code::LOST).unwrap();
    assert_eq!(ended.state, State::Ended(Outcome::Removed));
    v.plan(0, &[disk(1, 1)], Some(USER));
    v.started(1, 1, 4);
    let ended = v.exited(4, code::DAMAGED).unwrap();
    assert!(
        matches!(&ended.state, State::Ended(Outcome::Failed(s)) if s.contains("cannot be read"))
    );
    v.plan(0, &[disk(2, 1)], Some(USER));
    v.started(2, 1, 5);
    let ended = v.exited(5, code::FLUSH).unwrap();
    assert!(
        matches!(&ended.state, State::Ended(Outcome::Failed(s)) if s.contains("do not remove"))
    );
    assert!(v.exited(99, 0).is_none());
}

#[test]
fn at_most_eight_volumes() {
    let mut v = Volumes::new();
    let disks: Vec<DiskView> = (0..12).map(|i| disk(i, 1)).collect();
    assert_eq!(v.plan(0, &disks, Some(USER)).len(), MAX_VOLUMES);
}

#[test]
fn the_command_line_names_the_plug_and_owner() {
    assert_eq!(
        daemon_args("fatfuse", 3, 12, (1000, 1000)),
        ["fatfuse", "disk=3:12", "owner=1000:1000"]
    );
}

#[test]
fn disk_names_round_trip_and_nothing_else_parses() {
    for id in [0, 1, 7, 42, 999_999] {
        assert_eq!(parse_disk_name(&disk_name(id)), Some(id));
    }
    for bad in [
        "",
        "usb",
        "usb-1",
        "usb+1",
        "usb01",
        "usb1 ",
        " usb1",
        "USB1",
        "sda",
        "usb1x",
        "usb1234567",
        "usb0x10",
    ] {
        assert_eq!(parse_disk_name(bad), None, "{bad:?}");
    }
}

#[test]
fn a_mount_name_is_learned_once_per_daemon() {
    let mut v = Volumes::new();
    v.plan(0, &[disk(0, 1)], Some(USER));
    // Before the daemon has a pid there is nobody to name.
    assert!(!v.mounted_as(10, "MY_STICK"));
    v.started(0, 1, 10);
    assert!(v.mounted_as(10, "MY_STICK"));
    assert!(!v.mounted_as(10, "MY_STICK"));
    assert_eq!(v.entries()[0].name.as_deref(), Some("MY_STICK"));
    assert!(!v.mounted_as(99, "OTHER"));
}
