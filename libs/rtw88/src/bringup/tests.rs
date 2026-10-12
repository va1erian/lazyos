//! Bring-up tests on the healthy-chip model: the whole probe and start in
//! upstream's order, the facts read on the way, and that a failure leaves
//! the chip powered off. With `RTW88_FW` naming linux-firmware's
//! `rtw8822c_fw.bin`, the same runs with the real image.

use std::vec::Vec;

use super::*;
use crate::fake::FakeHci;
use crate::mac::regs::{CR, MAC_SEC_EN, RCR, SYS_CFG1};
use crate::testmodel::{healthy_chip, real_fw, rfe_efuse, synth_fw};

fn dev() -> Dev<FakeHci> {
    Dev::new(healthy_chip(rfe_efuse(1)))
}

fn probe_and_start(img: &[u8]) -> Dev<FakeHci> {
    let mut d = dev();
    d.chip_info_setup(img).unwrap();
    d.core_start(img).unwrap();
    d
}

#[test]
fn probe_reads_the_chip_and_leaves_it_off() {
    let img = synth_fw(0x200, 0x200, Some(0x100));
    let mut d = dev();
    d.chip_info_setup(&img).unwrap();
    assert_eq!(d.hal.chip_version, d.hci.peek32(SYS_CFG1));
    assert_eq!(
        (d.hal.cut_version, d.hal.rf_type, d.hal.rf_path_num),
        (1, RF_2T2R, 2)
    );
    assert_eq!(d.hal.antenna_tx, BB_PATH_AB);
    assert_eq!(d.efuse.hw_cap.nss, 2);
    assert_eq!(d.mac_addr(), FALLBACK_MAC, "blank efuse");
    assert_eq!((d.fw.version, d.fw.sub_version, d.fw.sub_index), (9, 9, 15));
    assert!(d.hal.rcr & RCR_VHT_DACK != 0);
    // hci setup, the download's ring reset, then stop at efuse_disable.
    assert_eq!(d.hci.calls, ["setup", "setup", "stop"]);
    assert_eq!(d.hci.regs[0x100], 0xEA, "powered off again");
    assert!(!d.hal.flags.power_on);
}

#[test]
fn core_start_brings_everything_up_in_order() {
    let img = synth_fw(0x200, 0x200, Some(0x100));
    let d = probe_and_start(&img);
    assert_eq!(
        d.hci.calls,
        [
            "setup",
            "setup",
            "stop",
            "setup",
            "setup",
            "interface_cfg",
            "start"
        ],
        "probe; power on: setup, ring reset after download, mac_init, hci start"
    );
    assert!(d.hal.flags.running && d.hal.flags.fw_running && d.hal.flags.power_on);
    // General info then PHY-DM info, sequence 0 and 1.
    assert_eq!(d.hci.h2c.len(), 2);
    assert_eq!((d.hci.h2c[0][48 + 2], d.hci.h2c[1][48 + 2]), (0x0D, 0x11));
    assert_eq!(d.hci.h2c[1][48 + 6], 1);
    assert_eq!(d.hci.peek32(RCR), d.hal.rcr);
    assert_eq!(
        d.hci.peek32(CR) & u32::from(MAC_SEC_EN),
        u32::from(MAC_SEC_EN)
    );
    assert!(d.sec.default_key_search);
}

#[test]
fn a_failed_start_powers_off() {
    let img = synth_fw(0x100, 0x100, None);
    let mut d = dev();
    d.chip_info_setup(&img).unwrap();
    d.hci.force.retain(|f| f.0 != 0x0080); // the firmware never comes up
    d.hci.log.clear();
    assert!(matches!(
        d.core_start(&img),
        Err(Error::FirmwareDownload(_))
    ));
    assert!(!d.hal.flags.running && !d.hal.flags.power_on);
    let disable = d.hci.writes().iter().any(|w| w.0 == 1 && w.1 == 0x93);
    assert!(disable, "the card disable flow ran");
}

#[test]
fn a_corrupt_image_is_refused_before_touching_the_chip() {
    let mut img = synth_fw(0x100, 0x100, None);
    img.pop();
    let mut d = dev();
    assert_eq!(
        d.chip_info_setup(&img),
        Err(Error::BadFirmware("section sizes do not match the file"))
    );
    assert!(d.core_start(&img).is_err());
    assert!(d.hci.log.is_empty());
}

#[test]
fn unsupported_rfe_fails_the_probe() {
    let mut d = Dev::new(healthy_chip(rfe_efuse(9)));
    let img = synth_fw(0x100, 0x100, None);
    assert_eq!(
        d.chip_info_setup(&img),
        Err(Error::Unsupported("rfe option"))
    );
    assert!(!d.hal.flags.power_on, "efuse_disable ran");
}

#[test]
fn stop_and_restart() {
    let img = synth_fw(0x100, 0x100, None);
    let mut d = probe_and_start(&img);
    d.core_stop();
    assert!(!d.hal.flags.running && !d.hal.flags.fw_running && !d.hal.flags.power_on);
    assert_eq!(d.hci.calls.last(), Some(&"stop"));
    d.core_start(&img).unwrap();
    assert!(d.hal.flags.running);
}

#[test]
fn real_firmware_downloads_when_given() {
    let Some(img) = real_fw() else {
        return;
    };
    let d = probe_and_start(&img);
    let fw = Firmware::parse(&img).unwrap();
    let chunks: usize = [Some(fw.dmem), Some(fw.imem), fw.emem]
        .into_iter()
        .flatten()
        .map(|s| s.data.len().div_ceil(0x1000))
        .sum();
    std::eprintln!("RTW88_FW: {} bytes, {chunks} chunks", img.len());
    // Each chunk staged twice: once at probe, once at start.
    assert_eq!(d.hci.rsvd.len(), 2 * chunks);
    let staged: Vec<u8> = d.hci.rsvd[..chunks]
        .iter()
        .flat_map(|p| p[48..].to_vec())
        .collect();
    assert_eq!(staged, &img[64..], "every section byte went down, in order");
}
