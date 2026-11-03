//! The Intel loader when the controller or the file misbehaves.

use std::vec::Vec;

use super::intel::*;
use super::{cc, vendor_event, Files};
use crate::step::{Config, Done, Failure, Input, Reason, Step};
use crate::Loader;

const RECOVERY: [u8; 8] = [1, 1, 1, 0, 0, 0, 0, 0];

fn good_tlv() -> (FakeIntel, Files, Vec<u8>) {
    let body = body((9, 9, 9));
    let tlv = tlv_version(0x19, 1, 0, (0x410, 0x410));
    let fake = FakeIntel::new(Vec::new(), tlv, body.len());
    let files = files(&[
        ("intel/ibt-0041-0041.sfi", sfi(true, &body)),
        ("intel/ibt-0041-0041.ddc", DDC.to_vec()),
    ]);
    (fake, files, body)
}

fn failure(step: &Step) -> Failure {
    match step {
        Step::Failed(f) => *f,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_refused_fragment_aborts_with_a_recovery_reset_and_sends_nothing_more() {
    let (mut fake, files, _) = good_tlv();
    fake.refuse_fragment = Some((4, 0x12));
    let (_, run) = play(&mut fake, &files);
    assert_eq!(
        run.count(SECURE_SEND),
        5,
        "the refused fragment is the last one sent"
    );
    assert_eq!(run.params(RESET), [&RECOVERY[..]]);
    assert_eq!(
        failure(&run.last),
        Failure {
            reason: Reason::CommandFailed {
                opcode: SECURE_SEND,
                status: 0x12
            },
            reenumerate: true
        }
    );
    assert_eq!(run.count(super::intel::WRITE_DDC), 0);
}

#[test]
fn a_rejected_image_is_recovered_from() {
    let (mut fake, files, _) = good_tlv();
    fake.verdict = Some(1);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(
        run.params(RESET),
        [&RECOVERY[..]],
        "no boot reset for a bad image"
    );
    assert_eq!(
        failure(&run.last).reason,
        Reason::DownloadRejected { result: 1 }
    );
    assert!(failure(&run.last).reenumerate);
}

#[test]
fn a_silent_bootloader_times_out_into_a_recovery_reset() {
    let (mut fake, files, _) = good_tlv();
    fake.verdict = None;
    let (mut loader, run) = play(&mut fake, &files);
    assert_eq!(run.last, Step::Wait);
    let deadline = loader.deadline().expect("waiting for the verdict");
    assert_eq!(
        loader.step(deadline - 1, Input::Tick),
        Step::Wait,
        "not yet"
    );
    let Step::Send(packet) = loader.step(deadline, Input::Tick) else {
        panic!("a recovery reset is due");
    };
    assert_eq!(&packet[3..], RECOVERY);
    let done = loader.step(deadline + 1, Input::Event(&cc(RESET, &[0])));
    assert_eq!(
        failure(&done),
        Failure {
            reason: Reason::Timeout { opcode: 0 },
            reenumerate: true
        }
    );
    // A second timeout cannot start another recovery.
    assert_eq!(loader.step(deadline * 3, Input::Tick), Step::Wait);
}

#[test]
fn a_recovery_the_device_never_answers_still_ends() {
    let (mut fake, files, _) = good_tlv();
    fake.verdict = None;
    let (mut loader, _) = play(&mut fake, &files);
    let t = loader.deadline().unwrap();
    assert!(matches!(loader.step(t, Input::Tick), Step::Send(_)));
    let t2 = loader.deadline().unwrap();
    assert_eq!(
        failure(&loader.step(t2, Input::Tick)),
        Failure {
            reason: Reason::Timeout { opcode: 0 },
            reenumerate: true
        }
    );
}

#[test]
fn a_boot_reset_the_controller_refuses_is_recovered_from() {
    let (mut fake, files, _) = good_tlv();
    fake.refuse_boot_reset = Some(0x0c);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(
        run.count(RESET),
        2,
        "the boot reset, then the recovery reset"
    );
    assert_eq!(run.params(RESET)[1], RECOVERY);
    assert_eq!(
        failure(&run.last),
        Failure {
            reason: Reason::CommandFailed {
                opcode: RESET,
                status: 0x0c
            },
            reenumerate: true
        }
    );
}

#[test]
fn no_boot_up_event_in_time_is_recovered_from() {
    let (mut fake, files, _) = good_tlv();
    fake.bootup = false;
    let (mut loader, _) = play(&mut fake, &files);
    let t = loader.deadline().unwrap();
    assert!(matches!(loader.step(t, Input::Tick), Step::Send(_)));
}

#[test]
fn a_missing_or_damaged_image_fails_before_anything_is_downloaded() {
    let (mut fake, _, _) = good_tlv();
    let (_, run) = play(&mut fake, &Files::new());
    assert_eq!(
        failure(&run.last),
        Failure {
            reason: Reason::FileMissing,
            reenumerate: false
        }
    );
    assert_eq!(run.count(SECURE_SEND), 0);

    let (mut fake, _, _) = good_tlv();
    let bad = files(&[("intel/ibt-0041-0041.sfi", std::vec![0u8; 700])]);
    let (_, run) = play(&mut fake, &bad);
    assert!(matches!(failure(&run.last).reason, Reason::BadFirmware(_)));
    assert_eq!(
        run.count(SECURE_SEND),
        0,
        "nothing is sent from a file that does not parse"
    );
    assert!(
        !failure(&run.last).reenumerate,
        "the bootloader was not touched"
    );
}

#[test]
fn controllers_this_crate_does_not_load_are_reported_not_guessed() {
    let cases: [(Vec<u8>, Vec<u8>); 5] = [
        // Newest generation (needs the unlocker / intermediate loader flow).
        (Vec::new(), tlv_version(0x1d, 1, 0, (0x410, 0x410))),
        // Intermediate loader image.
        (Vec::new(), tlv_version(0x19, 2, 0, (0x410, 0x410))),
        // A TLV answer for a platform that is not iBT.
        (Vec::new(), {
            let mut v = tlv_version(0x19, 1, 0, (0x410, 0x410));
            v[16] = 0x99; // the platform octet of the cnvi_bt entry
            v
        }),
        // The ROM-patch generation (.bseq files).
        (
            legacy_version(0x07, 0x06, (0, 0, 0)),
            legacy_version(0x07, 0x06, (0, 0, 0)),
        ),
        // An unknown firmware variant.
        (
            legacy_version(0x13, 0x55, (0, 0, 0)),
            legacy_version(0x13, 0x55, (0, 0, 0)),
        ),
    ];
    for (legacy, tlv) in cases {
        let mut fake = FakeIntel::new(legacy, tlv, 0);
        let (_, run) = play(&mut fake, &Files::new());
        let Step::Failed(f) = run.last else {
            panic!("expected a failure, got {:?}", run.last);
        };
        assert!(
            matches!(
                f.reason,
                Reason::Unsupported(_) | Reason::BadResponse { .. }
            ),
            "{:?}",
            f.reason
        );
        assert!(!f.reenumerate);
        assert_eq!(run.count(SECURE_SEND), 0);
    }
}

#[test]
fn a_bootloader_that_does_not_ack_every_fragment_is_refused() {
    let mut fake = FakeIntel::new(
        legacy_version(0x13, 0x06, (0, 0, 0)),
        legacy_version(0x13, 0x06, (0, 0, 0)),
        0,
    );
    fake.boot_params = boot_params(0, 1);
    let (_, run) = play(&mut fake, &Files::new());
    assert!(matches!(failure(&run.last).reason, Reason::Unsupported(_)));
}

#[test]
fn a_refused_read_version_and_a_short_answer_fail_the_load() {
    let mut loader = Loader::new(crate::Vendor::Intel);
    let Step::Send(_) = loader.step(0, Input::Start) else {
        panic!()
    };
    let step = loader.step(1, Input::Event(&cc(READ_VERSION, &[1])));
    assert_eq!(
        failure(&step).reason,
        Reason::CommandFailed {
            opcode: READ_VERSION,
            status: 1
        }
    );
    // A TLV list cut in the middle of an entry.
    let (_, run) = play(
        &mut FakeIntel::new(Vec::new(), std::vec![0, 0x10, 4, 1, 2], 0),
        &Files::new(),
    );
    assert!(matches!(
        failure(&run.last).reason,
        Reason::BadResponse { .. }
    ));
}

#[test]
fn a_command_that_gets_no_answer_times_out_and_a_detach_ends_the_load() {
    let mut loader = Loader::new(crate::Vendor::Intel);
    let Step::Send(_) = loader.step(100, Input::Start) else {
        panic!()
    };
    let t = loader.deadline().unwrap();
    assert_eq!(t, 100 + 5_000);
    assert_eq!(loader.step(t - 1, Input::Tick), Step::Wait);
    assert_eq!(
        failure(&loader.step(t, Input::Tick)).reason,
        Reason::Timeout {
            opcode: READ_VERSION
        }
    );

    let mut loader = Loader::new(crate::Vendor::Intel);
    let _ = loader.step(0, Input::Start);
    assert_eq!(
        failure(&loader.step(1, Input::Detached)).reason,
        Reason::Detached
    );
}

#[test]
fn a_detach_during_the_download_expects_the_device_to_come_back() {
    let (mut fake, files, _) = good_tlv();
    fake.verdict = None;
    let (mut loader, _) = play(&mut fake, &files);
    let f = failure(&loader.step(9, Input::Detached));
    assert_eq!(
        f,
        Failure {
            reason: Reason::Detached,
            reenumerate: true
        }
    );
}

#[test]
fn events_that_are_not_ours_are_ignored() {
    let (mut fake, files, _) = good_tlv();
    fake.verdict = None;
    let (mut loader, _) = play(&mut fake, &files);
    for noise in [
        std::vec![],
        std::vec![0x0e],
        std::vec![0x0e, 3, 1, 0x09, 0xfc],
        cc(0x1234, &[0]),
        cc(SECURE_SEND, &[0]),
        vendor_event(&[0x06, 0, 0]),
        vendor_event(&[0x02, 0, 0, 0, 0, 0, 0]),
        vendor_event(&[]),
        std::vec![0x3e, 1, 1],
    ] {
        assert_eq!(
            loader.step(10, Input::Event(&noise)),
            Step::Wait,
            "{noise:?}"
        );
    }
    assert_eq!(loader.step(11, Input::Start), Step::Wait);
    assert_eq!(loader.step(12, Input::NoFile), Step::Wait);
}

#[test]
fn the_device_configuration_file_is_optional_and_checked_whole() {
    // Damaged: nothing is written.
    let (mut fake, mut files, _) = good_tlv();
    files.insert("intel/ibt-0041-0041.ddc".into(), std::vec![5, 1, 2]);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(run.count(WRITE_DDC), 0);
    assert_eq!(
        run.last,
        Step::Done(Done::Operational {
            downloaded: true,
            config: Config::Failed
        })
    );
    // The controller refuses the second record: the load still succeeds.
    let (mut fake, files, _) = good_tlv();
    fake.refuse_ddc = Some(1);
    let (_, run) = play(&mut fake, &files);
    assert_eq!(run.count(WRITE_DDC), 2);
    assert!(matches!(
        run.last,
        Step::Done(Done::Operational {
            config: Config::Failed,
            ..
        })
    ));
    // An empty file is a valid, empty configuration.
    let (mut fake, mut files, _) = good_tlv();
    files.insert("intel/ibt-0041-0041.ddc".into(), Vec::new());
    let (_, run) = play(&mut fake, &files);
    assert!(matches!(
        run.last,
        Step::Done(Done::Operational {
            config: Config::Applied(0),
            ..
        })
    ));
}
