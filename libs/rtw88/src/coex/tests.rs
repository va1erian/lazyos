//! Coexistence bring-up on the fake bus with a model of the LTE coex
//! indirect window (0x1700 control, 0x1704 write data, 0x1708 read data),
//! checked against the end state in the module docs.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use super::*;
use crate::fake::FakeHci;

type Indirect = Rc<RefCell<BTreeMap<u32, u32>>>;

fn word(regs: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([regs[at], regs[at + 1], regs[at + 2], regs[at + 3]])
}

/// A device whose LTE coex window works and whose scoreboard reads
/// `scbd` (Bluetooth's bits).
fn dev(share_ant: bool, scbd: u16) -> (Dev<FakeHci>, Indirect) {
    let mut d = Dev::new(FakeHci::default());
    d.hal.rf_path_num = 2;
    d.hal.rf_phy_num = 2;
    d.efuse.share_ant = share_ant;
    d.efuse.rfe_option = 1;
    d.hci.write16(REG_WIFI_BT_INFO, scbd);
    d.hci.force.push((0x1700, 1 << 29, 1 << 29)); // LTECOEX_READY
    let map: Indirect = Rc::default();
    let m = map.clone();
    d.hci.hooks.push(std::boxed::Box::new(
        move |regs: &mut [u8], width, addr, val| {
            if width != 4 || addr != 0x1700 {
                return;
            }
            let off = val & 0xffff;
            match val & 0xffff_0000 {
                0xc00f_0000 => {
                    m.borrow_mut().insert(off, word(regs, 0x1704));
                }
                0x800f_0000 => {
                    let v = m.borrow().get(&off).copied().unwrap_or(0);
                    regs[0x1708..0x170c].copy_from_slice(&v.to_le_bytes());
                }
                _ => {}
            }
        },
    ));
    (d, map)
}

/// The command words sent through the H2C mailboxes, in order.
fn h2c_words(d: &Dev<FakeHci>) -> Vec<u32> {
    d.hci
        .writes()
        .iter()
        .filter(|&&(w, a, _)| w == 4 && (0x1d0..=0x1dc).contains(&a))
        .map(|&(_, _, v)| v)
        .collect()
}

#[test]
fn wifi_only_gives_wifi_the_antenna() {
    let (mut d, map) = dev(true, 0);
    power_on_setting(&mut d);
    init_hw_config(&mut d, true);
    let lte = map.borrow();
    // GNT_BT forced low (01) in [15:14] and [11:10], GNT_WL forced high
    // (11) in [13:12] and [9:8], LTE coexistence off (bit 7).
    assert_eq!(lte[&0x38] & 0xff80, 0x7700);
    assert_eq!(lte[&0xa0] & 0xffff, 0xffff);
    assert_eq!(lte[&0xa4] & 0xffff, 0xffff);
    let h = &d.hci;
    assert_eq!(
        h.peek32(0x70) >> 24 & 0x4,
        0x4,
        "antenna path mux owned by Wi-Fi"
    );
    assert_eq!(
        h.peek32(0xa8) >> 16,
        0x8003,
        "scoreboard: active, on, BT interrupt"
    );
    assert_eq!(h.peek32(0x6c0), 0x5555_5555);
    assert_eq!(h.peek32(0x6c4), 0x5555_5555);
    assert_eq!(h.peek32(0x6c8), 0xf0ff_ffff);
    assert_eq!(
        h.peek32(0x6cc) & 0x0800_0018,
        0x0800_0018,
        "TX resp, beacon, beacon queue high priority"
    );
    assert_eq!(h.peek32(0x40) & 0x220, 0x220, "PTA on, BT PTA pins");
    assert_eq!(h.peek32(0x4c04) & 0xfffff, 0x40000, "RF B register 1");
    assert_eq!(h.peek32(0xff18) >> 16 & 0xff, 0, "red x");
    // H2C: 0x69 (slot extend off: op 0xc, data 1), then 0x60 (TDMA off);
    // Bluetooth is off, so no BT info query.
    assert_eq!(h2c_words(&d), [0x0001_0c69, 0x0000_0060]);
    assert!(d.coex.stop_dm && d.coex.bt_disabled);
    assert_eq!(d.coex.rfe_module_type, 1);

    power_off_setting(&mut d);
    assert_eq!(d.hci.peek32(0xa8) >> 16, 0x8000);
}

#[test]
fn upstream_default_with_bluetooth_alive_starves_wifi() {
    // `wifi_only = false` (upstream's choice on a combo card) while BT has
    // set its on/off bit: GNT_BT high, GNT_WL low until a coexistence
    // mechanism LazyOS does not run would hand the grants to the PTA.
    let (mut d, map) = dev(true, SCBD_ONOFF);
    power_on_setting(&mut d);
    init_hw_config(&mut d, false);
    assert_eq!(map.borrow()[&0x38] & 0xff00, 0xdd00);
    assert!(!d.coex.stop_dm && d.coex.freeze);
    // The BT info query goes out since Bluetooth is up.
    assert!(h2c_words(&d).contains(&0x0000_0161));
}

#[test]
fn non_shared_antenna_writes_no_table_and_no_tdma() {
    let (mut d, map) = dev(false, 0);
    d.hci.write32(0x6c0, 0x1234_5678);
    power_on_setting(&mut d);
    init_hw_config(&mut d, true);
    assert_eq!(
        d.hci.peek32(0x6c0),
        0x1234_5678,
        "case 1 - 100 wraps past the table"
    );
    assert_eq!(h2c_words(&d), [0x0001_0c69]);
    assert_eq!(map.borrow()[&0x38] & 0xff00, 0x7700);
    assert!(!d.coex.wlg_at_btg);
}

#[test]
fn a_dead_indirect_window_drops_the_writes() {
    let (mut d, map) = dev(true, 0);
    d.hci.force.clear();
    power_on_setting(&mut d);
    init_hw_config(&mut d, true);
    assert!(map.borrow().is_empty());
}

#[test]
fn the_grant_check_reads_back_a_wifi_only_init() {
    let (mut d, _map) = dev(true, 0);
    power_on_setting(&mut d);
    init_hw_config(&mut d, true);
    let check = check_wifi_grant(&mut d);
    assert!(check.ok(), "{check:?}");
    assert_eq!(d.coex.grant, Some(check));
}

#[test]
fn a_coex_window_that_never_answers_is_counted_not_hidden() {
    // A bare fake: LTECOEX_READY never sets, so every indirect access fails.
    let mut d = crate::Dev::new(crate::fake::FakeHci::default());
    init_hw_config(&mut d, true);
    assert!(d.coex.indirect_errors > 0);
    let check = check_wifi_grant(&mut d);
    assert!(!check.ok());
    assert_eq!(check.lte_ctrl, None);
}
