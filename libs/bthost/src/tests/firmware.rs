//! The firmware phase against `btsim`'s vendor radios: an Intel part that
//! boots in its bootloader (legacy and TLV flavours), a Realtek ROM, the
//! recovery reset that makes the part re-enumerate, the retry cap, and the
//! missing-file cases.

use std::vec;
use std::vec::Vec;

use btbond::BondStore;
use btsim::{Part, RadioFaults};

use super::fw_images::{intel_body, real, realtek_patch};
use super::fw_rig::{intel_files, FwRig};
use crate::{Config, Effect, Host, UsbIds, MAX_REENUMERATIONS};

#[test]
fn an_intel_bootloader_part_gets_its_firmware_before_init_runs() {
    let mut rig = FwRig::new(
        Part::IntelAx201,
        RadioFaults::default(),
        &intel_files(false),
    );
    rig.up();
    assert!(
        rig.seen("FW:VENDOR vendor=intel usb=8087:0026"),
        "{:?}",
        rig.traces
    );
    assert_eq!(
        rig.asked,
        [
            ("intel/ibt-19-0-4.sfi".into(), true),
            ("intel/ibt-19-0-4.ddc".into(), false)
        ]
    );
    assert!(rig.seen("FW:DONE vendor=intel downloaded=1 config=applied:2 ms="));
    let done = rig.traces.iter().position(|t| t.starts_with("FW:DONE"));
    let pass = rig.traces.iter().position(|t| t.starts_with("CTRL:PASS"));
    assert!(
        done < pass,
        "init runs after the download: {:?}",
        rig.traces
    );
    let stats = rig.sim.radio_stats().unwrap();
    assert!(stats.operational);
    assert_eq!(stats.stream_bytes, intel_body().len());
    assert_eq!(stats.ddc_records, 2);
    assert_eq!(rig.hosts, 1, "a successful download does not re-enumerate");
    assert!(
        rig.sim.violations().is_empty(),
        "{:?}",
        rig.sim.violations()
    );
    assert!(rig.failed.is_none());
}

#[test]
fn the_first_hci_reset_comes_after_the_firmware() {
    use bthci::Command;
    let mut rig = FwRig::new(
        Part::IntelAx201,
        RadioFaults::default(),
        &intel_files(false),
    );
    rig.up();
    let reset = rig
        .sim
        .commands_seen()
        .iter()
        .position(|c| matches!(c, Command::Reset))
        .expect("init sent HCI_Reset");
    let last_vendor = rig
        .sim
        .commands_seen()
        .iter()
        .rposition(|c| matches!(c, Command::Unknown { .. }))
        .unwrap();
    assert!(last_vendor < reset);
}

#[test]
fn a_tlv_intel_part_takes_both_signature_blocks() {
    let mut rig = FwRig::new(Part::IntelAx211, RadioFaults::default(), &intel_files(true));
    rig.up();
    assert!(
        rig.seen("FW:DONE vendor=intel downloaded=1 config=applied:2"),
        "{:?}",
        rig.traces
    );
    assert_eq!(rig.asked[0].0, "intel/ibt-0040-0041.sfi");
    assert!(rig.seen("CTRL:PASS"));
}

#[test]
fn a_missing_optional_config_does_not_stop_the_part() {
    let files = &intel_files(false)[..1];
    let mut rig = FwRig::new(Part::IntelAx201, RadioFaults::default(), files);
    rig.up();
    assert!(rig.seen("FW:FILE name=intel/ibt-19-0-4.ddc missing=1"));
    assert!(
        rig.seen("FW:DONE vendor=intel downloaded=1 config=missing"),
        "{:?}",
        rig.traces
    );
    assert!(rig.seen("CTRL:PASS"));
}

#[test]
fn a_missing_required_file_fails_before_any_reset() {
    use bthci::Command;
    let mut rig = FwRig::new(Part::IntelAx201, RadioFaults::default(), &[]);
    rig.up();
    assert!(rig.seen("FW:FAIL reason=file-missing"), "{:?}", rig.traces);
    assert!(!rig.seen("CTRL:PASS"));
    assert!(rig.failed.is_some());
    assert!(!rig
        .sim
        .commands_seen()
        .iter()
        .any(|c| matches!(c, Command::Reset)));
    assert_eq!(rig.host.next_deadline(), None, "a failed host sleeps");
}

#[test]
fn a_garbage_firmware_file_fails_without_sending_it() {
    let files = [("intel/ibt-19-0-4.sfi", std::vec![0xA5; 3000])];
    let mut rig = FwRig::new(Part::IntelAx201, RadioFaults::default(), &files);
    rig.up();
    assert!(rig.seen("FW:FAIL reason=bad-firmware"), "{:?}", rig.traces);
    assert_eq!(rig.sim.radio_stats().unwrap().fragments, 0);
}

#[test]
fn a_rejected_image_resets_the_part_which_returns_and_loads_again() {
    let faults = RadioFaults {
        reject_downloads: 1,
        ..RadioFaults::default()
    };
    let mut rig = FwRig::new(Part::IntelAx201, faults, &intel_files(false));
    rig.up();
    assert!(rig.seen("FW:REENUM attempt=1"), "{:?}", rig.traces);
    assert_eq!(rig.hosts, 2);
    assert_eq!(rig.count("FW:VENDOR"), 2);
    assert!(rig.seen("FW:DONE vendor=intel downloaded=1"));
    assert!(rig.seen("CTRL:PASS"));
    let stats = rig.sim.radio_stats().unwrap();
    assert_eq!(stats.detaches, 1);
    assert!(stats.operational);
    assert_eq!(
        rig.host.firmware_attempts(),
        0,
        "cleared once the part is up"
    );
}

#[test]
fn a_part_that_reboots_into_the_image_and_returns_is_up_the_second_time() {
    let faults = RadioFaults {
        reenumerate_after_load: true,
        ..RadioFaults::default()
    };
    let mut rig = FwRig::new(Part::IntelAx201, faults, &intel_files(false));
    rig.up();
    assert!(rig.seen("FW:REENUM attempt=1"), "{:?}", rig.traces);
    // The second start finds the operational firmware running.
    assert!(
        rig.seen("FW:DONE vendor=intel downloaded=0"),
        "{:?}",
        rig.traces
    );
    assert!(rig.seen("CTRL:PASS"));
}

#[test]
fn a_part_that_keeps_resetting_stops_at_the_cap() {
    let faults = RadioFaults {
        reject_downloads: 100,
        ..RadioFaults::default()
    };
    let mut rig = FwRig::new(Part::IntelAx201, faults, &intel_files(false));
    rig.up();
    assert_eq!(rig.count("FW:REENUM"), MAX_REENUMERATIONS as usize);
    // The host that gave up could not stop the part leaving; the next one
    // touches nothing, so the loop ends and the count starts afresh.
    assert_eq!(rig.count("FW:FAIL reason=reenumeration-cap"), 2);
    assert_eq!(rig.host.firmware_attempts(), 0);
    assert!(
        rig.seen("FW:FAIL reason=reenumeration-cap"),
        "{:?}",
        rig.traces
    );
    assert!(!rig.seen("CTRL:PASS"), "{:?}", rig.traces);
    assert!(rig.failed.is_some());
    assert_eq!(rig.hosts, MAX_REENUMERATIONS + 2);
}

#[test]
fn a_detach_in_the_middle_of_a_download_counts_and_waits() {
    let mut rig = FwRig::new(
        Part::IntelAx201,
        RadioFaults::default(),
        &intel_files(false),
    );
    rig.run(1_000_000, |r| r.sim.radio_stats().unwrap().fragments >= 3);
    assert!(rig.sim.radio_stats().unwrap().fragments >= 3);
    rig.host.on_detached(rig.now);
    while let Some(effect) = rig.host.poll_effect() {
        if let Effect::Trace(text) = effect {
            rig.traces.push(text);
        }
    }
    assert!(rig.seen("FW:REENUM attempt=1"), "{:?}", rig.traces);
    assert_eq!(rig.host.firmware_attempts(), 1);
    assert!(
        rig.host.poll_command(rig.now).is_none(),
        "nothing more is sent"
    );
}

#[test]
fn the_wait_for_the_device_to_leave_is_a_deadline_not_a_poll() {
    let faults = RadioFaults {
        reject_downloads: 1,
        ..RadioFaults::default()
    };
    let mut rig = FwRig::new(Part::IntelAx201, faults, &intel_files(false));
    rig.hold_detach = true;
    rig.run(1_000_000, |r| r.seen("FW:REENUM"));
    // The recovery reset went out and was answered; the part has not left.
    let deadline = rig.host.next_deadline().expect("a bounded wait");
    assert!(deadline > rig.now, "a wait, not a poll");
    assert_eq!(rig.host.poll_command(rig.now), None);
    rig.now = deadline;
    rig.host.on_tick(rig.now);
    while let Some(Effect::Trace(text)) = rig.host.poll_effect() {
        rig.traces.push(text);
    }
    assert!(
        rig.seen("FW:FAIL reason=no-reenumeration"),
        "{:?}",
        rig.traces
    );
    assert_eq!(rig.host.next_deadline(), None);
}

#[test]
fn a_realtek_rom_gets_its_patch_and_board_config() {
    let files = [
        ("rtl_bt/rtl8761bu_fw.bin", realtek_patch()),
        (
            "rtl_bt/rtl8761bu_config.bin",
            std::vec![0x55, 0xab, 0x23, 0x87, 0, 0],
        ),
    ];
    let mut rig = FwRig::new(Part::Rtl8761bu, RadioFaults::default(), &files);
    rig.up();
    assert!(
        rig.seen("FW:VENDOR vendor=realtek usb=0bda:8771"),
        "{:?}",
        rig.traces
    );
    assert_eq!(rig.asked[0], ("rtl_bt/rtl8761bu_fw.bin".into(), true));
    assert!(
        rig.seen("FW:DONE vendor=realtek downloaded=1 config=none"),
        "{:?}",
        rig.traces
    );
    assert!(
        rig.seen("CTRL:PASS:hci=10 lmp=10 vendor=0x005d"),
        "{:?}",
        rig.traces
    );
    assert!(rig.sim.radio_stats().unwrap().fragments >= 3);
}

#[test]
fn a_realtek_download_the_part_refuses_fails_the_load() {
    let faults = RadioFaults {
        refuse_fragment: Some(1),
        ..RadioFaults::default()
    };
    let files = [("rtl_bt/rtl8761bu_fw.bin", realtek_patch())];
    let mut rig = FwRig::new(Part::Rtl8761bu, faults, &files);
    rig.up();
    assert!(
        rig.seen("FW:FAIL reason=command-failed opcode=0xfc20"),
        "{:?}",
        rig.traces
    );
    assert!(!rig.seen("CTRL:PASS"));
}

#[test]
fn a_silent_controller_times_out_at_the_loaders_deadline() {
    let mut host = Host::new_for(
        Config::default(),
        BondStore::new(),
        0,
        Some(UsbIds {
            vendor: 0x0bda,
            product: 0xc822,
        }),
        0,
    );
    // The first command goes out; nobody answers.
    assert!(host.poll_command(0).is_some());
    let due = host.next_deadline().expect("the command's deadline");
    assert!(due > 0 && due <= 6_000);
    host.on_tick(due);
    let mut text = Vec::new();
    while let Some(effect) = host.poll_effect() {
        if let Effect::Trace(t) = effect {
            text.push(t);
        }
    }
    assert!(
        text.iter().any(|t| t.starts_with("FW:FAIL reason=timeout")),
        "{text:?}"
    );
    assert_eq!(host.next_deadline(), None);
}

#[test]
fn a_generic_controller_goes_straight_to_init_with_no_firmware_lines() {
    let mut rig = FwRig::generic();
    rig.up();
    assert!(rig.seen("CTRL:PASS"));
    assert!(!rig.seen("FW:"), "{:?}", rig.traces);
    assert!(rig.asked.is_empty());
}

#[test]
fn hostile_events_during_the_download_are_ignored() {
    let mut host = Host::new_for(
        Config::default(),
        BondStore::new(),
        0,
        Some(UsbIds {
            vendor: 0x8087,
            product: 0x0026,
        }),
        0,
    );
    for junk in [
        &[][..],
        &[0xff][..],
        &[0x0e, 0xff, 1, 2][..],
        &[0xff, 200, 1][..],
    ] {
        host.on_event(1, junk);
        host.on_acl(1, junk);
    }
    // A file nobody asked for is not fed to the loader either.
    host.on_firmware_file(2, Some(std::vec![1, 2, 3]));
    assert!(host.loading_firmware());
}

#[test]
fn the_real_intel_and_realtek_files_download_to_the_simulated_parts() {
    let cases = [
        (
            Part::IntelAx201,
            vec!["intel/ibt-19-0-4.sfi", "intel/ibt-19-0-4.ddc"],
        ),
        (
            Part::IntelAx211,
            vec!["intel/ibt-0040-0041.sfi", "intel/ibt-0040-0041.ddc"],
        ),
        (Part::Rtl8822cu, vec!["rtl_bt/rtl8822cu_fw.bin"]),
    ];
    for (part, names) in cases {
        let files: Vec<(&str, Vec<u8>)> = names
            .iter()
            .filter_map(|&n| real(n).map(|d| (n, d)))
            .collect();
        if files.len() != names.len() {
            continue;
        }
        let mut rig = FwRig::new(part, RadioFaults::default(), &files);
        rig.up();
        assert!(
            rig.seen("FW:DONE"),
            "{part:?}: {:?}",
            &rig.traces[..rig.traces.len().min(8)]
        );
        assert!(rig.seen("CTRL:PASS"), "{part:?}");
        assert!(rig.sim.violations().is_empty(), "{part:?}");
    }
}
