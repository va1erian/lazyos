//! MAC tests against the fake bus: the power sequence written as upstream's
//! tables say, the TX FIFO carve-up, `mac_init`'s registers, the firmware
//! download's DDMA program, and the failure paths.

use std::vec::Vec;

use super::regs::*;
use super::*;
use crate::dev::{Bw, Dev};
use crate::fake::{Access, FakeHci};
use crate::fw::Firmware;
use crate::rtw8822c::spec::CARD_ENABLE_FLOW_8822C;
use crate::testmodel::{healthy_chip, rfe_efuse, synth_fw};

fn dev() -> Dev<FakeHci> {
    Dev::new(healthy_chip(rfe_efuse(1)))
}

/// Byte writes to the power-sequence registers, in order.
fn byte_writes(d: &Dev<FakeHci>) -> Vec<(u32, u32)> {
    d.hci
        .writes()
        .into_iter()
        .filter(|w| w.0 == 1)
        .map(|w| (w.1, w.2))
        .collect()
}

#[test]
fn enable_flow_on_pcie_writes_upstreams_steps() {
    let mut d = dev();
    d.hal.cut_version = 1;
    pwr_seq_parser(&mut d, &CARD_ENABLE_FLOW_8822C).unwrap();
    // trans_carddis_to_cardemu then trans_cardemu_to_act, SDIO/USB steps
    // skipped, each a read-modify-write of one byte from a zeroed chip
    // (0x06 reads bit 1 set: the forced power-ready bit).
    let want: &[(u32, u32)] = &[
        (0x2E, 0x04),
        (0x2D, 0x00),
        (0x7F, 0x00),
        (0x05, 0x00),
        (0x05, 0x00),
        (0x75, 0x01),
        (0x75, 0x00),
        (0x2E, 0x04),
        (0x06, 0x03),
        (0x05, 0x00),
        (0x1018, 0x04),
        (0x05, 0x01),
        (0x74, 0x20),
        (0x71, 0x00),
        (0x62, 0xE0),
        (0x61, 0x00),
        (0x1F, 0x80),
        (0xEF, 0x80),
        (0x1045, 0x10),
        (0x10, 0x04),
        (0x1064, 0x02),
    ];
    assert_eq!(byte_writes(&d), want);
}

#[test]
fn power_on_cold_chip() {
    let mut d = dev();
    power_on(&mut d).unwrap();
    assert!(d.hal.flags.power_on);
    let w = d.hci.writes();
    assert_eq!(
        w[0],
        (1, RSV_CTRL, 0),
        "pre_system_cfg starts with REG_RSV_CTRL"
    );
    // init_system_cfg: DDMA on, the chip's function enables, CR_EXT.
    assert_eq!(
        d.hci.peek32(CPU_DMEM_CON) & (WL_PLATFORM_RST | DDMA_EN),
        WL_PLATFORM_RST | DDMA_EN
    );
    assert_eq!(d.hci.regs[SYS_FUNC_EN as usize + 1] & 0xD8, 0xD8);
    assert_eq!(d.hci.regs[CR_EXT as usize + 3] & 0x0F, 0x0C);
    // BB/RF held in reset.
    assert_eq!(d.hci.regs[RF_CTRL as usize] & 0x7, 0);
}

#[test]
fn power_on_a_powered_chip_cycles_it() {
    let mut d = dev();
    d.hci.regs[0x100] = 0x00; // left on by a previous driver
    power_on(&mut d).unwrap();
    let w = byte_writes(&d);
    let first_disable = w
        .iter()
        .position(|&(a, _)| a == 0x93)
        .expect("disable flow ran");
    let first_enable = w
        .iter()
        .position(|&(a, _)| a == 0x2E)
        .expect("enable flow ran");
    assert!(first_disable < first_enable);
    assert!(d.hal.flags.power_on);
}

#[test]
fn power_poll_timeout_retries_with_pfm_wowl_then_fails() {
    let mut d = dev();
    d.hci.force.retain(|f| f.0 != 0x0004);
    let err = power_on(&mut d).unwrap_err();
    assert_eq!(
        err,
        Error::Timeout {
            step: "pwr seq poll",
            addr: 0x06
        }
    );
    let toggles: Vec<u32> = byte_writes(&d)
        .into_iter()
        .filter(|&(a, _)| a == SYS_PW_CTRL)
        .map(|w| w.1)
        .collect();
    assert_eq!(toggles, [u32::from(PFM_WOWL), 0]);
    assert!(!d.hal.flags.power_on);
}

#[test]
fn trx_fifo_layout_matches_upstream_numbers() {
    let mut f = FifoConf::default();
    set_trx_fifo_info(&mut f).unwrap();
    assert_eq!(f.txff_pg_num, 2048);
    assert_eq!(f.rsvd_pg_num, 110);
    assert_eq!(f.acq_pg_num, 1938);
    assert_eq!(f.rsvd_boundary, 1938);
    assert_eq!(f.rsvd_drv_addr, 1938);
    assert_eq!(f.rsvd_h2c_info_addr, 1954);
    assert_eq!(f.rsvd_h2c_sta_info_addr, 1978);
    assert_eq!(f.rsvd_h2cq_addr, 1986);
    assert_eq!(f.rsvd_fw_txbuf_addr, 1994);
    assert_eq!(f.rsvd_csibuf_addr, 1998);
}

#[test]
fn mac_init_programs_queues_and_h2c() {
    let mut d = dev();
    mac_init(&mut d).unwrap();
    let r = |a: u32| d.hci.peek32(a);
    // hi=High(3), mg=Extra(0), bk=Low(1), be=Low(1), vi=Normal(2), vo=Normal(2)
    assert_eq!(r(TXDMA_PQ_MAP) & 0xffff, 0xC5A0);
    assert_eq!(d.hci.regs[CR as usize], MAC_TRX_ENABLE);
    assert_eq!(r(FIFOPAGE_INFO[0]) & 0xffff, 64);
    assert_eq!(r(FIFOPAGE_INFO[4]) & 0xffff, 1938 - 4 * 64 - 1);
    assert_eq!(r(FIFOPAGE_CTRL_2) & 0xffff, 1938);
    assert_eq!(r(RXFF_BNDY), 24576 - 256 - 1);
    assert_eq!(r(H2C_HEAD), 1986 << 7);
    assert_eq!(r(H2C_TAIL), (1986 << 7) + 1024);
    assert_eq!(d.hci.regs[RX_DRVINFO_SZ as usize], PHY_STATUS_SIZE);
    assert_eq!(r(RCR) & RCR_APP_PHYSTS, RCR_APP_PHYSTS);
    assert_eq!(d.hci.calls, ["interface_cfg"]);
    assert_eq!(d.hal.fifo.rqpn, Some(crate::rtw8822c::spec::RQPN_PCIE));
}

#[test]
fn mac_init_fails_on_a_busy_h2c_queue_or_llt() {
    let mut d = dev();
    d.hci.regs[H2C_PKT_WRITEADDR as usize] = 0x10;
    assert_eq!(mac_init(&mut d), Err(Error::Invalid("H2C queue mismatch")));

    let mut d = dev();
    d.hci.force.retain(|f| f.0 != AUTO_LLT_V1);
    assert!(matches!(
        mac_init(&mut d),
        Err(Error::Timeout {
            step: "auto llt init",
            ..
        })
    ));
}

#[test]
fn set_channel_mac_80mhz_on_5ghz() {
    let mut f = FakeHci::default();
    set_channel_mac(&mut f, 42, Bw::Mhz80, 1);
    assert_eq!(f.regs[DATA_SC as usize], 0x91);
    assert_eq!(f.peek32(WMAC_TRXPTCL_CTL) & RFMOD, RFMOD_80M);
    assert_eq!(f.regs[CCK_CHECK as usize] & CHECK_CCK_EN, CHECK_CCK_EN);
    assert_eq!(f.regs[USTIME_TSF as usize], 80);
    set_channel_mac(&mut f, 6, Bw::Mhz20, 0);
    assert_eq!(f.regs[DATA_SC as usize], 0);
    assert_eq!(f.peek32(WMAC_TRXPTCL_CTL) & RFMOD, 0);
    assert_eq!(f.regs[CCK_CHECK as usize] & CHECK_CCK_EN, 0);
}

/// The DDMA programs (source, destination, control) in order.
fn ddma_programs(f: &FakeHci) -> Vec<(u32, u32, u32)> {
    let w = f.writes();
    let mut out = Vec::new();
    for (i, x) in w.iter().enumerate() {
        if x.1 == DDMA_CH0SA
            && i + 2 < w.len()
            && w[i + 1].1 == DDMA_CH0DA
            && w[i + 2].1 == DDMA_CH0CTRL
        {
            out.push((x.2, w[i + 1].2, w[i + 2].2));
        }
    }
    out
}

#[test]
fn firmware_download_programs_the_ddma_per_chunk() {
    let img = synth_fw(0x1800, 0x100, Some(0x10));
    let fw = Firmware::parse(&img).unwrap();
    let mut d = dev();
    download_firmware(&mut d, &fw).unwrap();

    let p = ddma_programs(&d.hci);
    let src = OCPBASE_TXBUF_88XX + 48;
    let own_ck = DDMACH0_OWN | DDMACH0_CHKSUM_EN;
    assert_eq!(
        p,
        [
            (src, 0x0020_0000, own_ck | 0x1000),
            (src, 0x0020_1000, own_ck | DDMACH0_CHKSUM_CONT | 0x808),
            (src, 0x0003_0000, own_ck | 0x108),
            (src, 0x0010_0000, own_ck | 0x18),
        ]
    );
    // Each chunk was staged through the beacon queue with its descriptor.
    assert_eq!(d.hci.rsvd.len(), 4);
    assert_eq!(d.hci.rsvd[0].len(), 48 + 0x1000);
    assert_eq!(&d.hci.rsvd[1][48..], &img[64 + 0x1000..64 + 0x1808]);
    let w1 = u32::from_le_bytes(d.hci.rsvd[0][4..8].try_into().unwrap());
    assert_eq!((w1 >> 8) & 0x1f, 16, "QSEL_BEACON");
    assert!(d.hal.flags.fw_running);
    assert_eq!(d.hci.calls, ["setup"], "rings reset after the download");
    // The CPU is enabled again and the download bit off.
    assert_eq!(d.hci.regs[SYS_FUNC_EN as usize + 1] & FEN_CPUEN, FEN_CPUEN);
}

#[test]
fn firmware_download_failures() {
    let img = synth_fw(0x100, 0x100, None);
    let fw = Firmware::parse(&img).unwrap();

    // A checksum the DDMA rejects.
    let mut d = dev();
    d.hci.force.retain(|f| f.0 != DDMA_CH0CTRL);
    d.hci
        .force
        .push((DDMA_CH0CTRL, (1 << 31) | (1 << 27), 1 << 27));
    assert_eq!(
        download_firmware(&mut d, &fw),
        Err(Error::FirmwareDownload("invalid fw checksum"))
    );
    assert!(!d.hal.flags.fw_running);
    assert_eq!(
        d.hci.regs[MCUFW_CTRL as usize] & MCUFWDL_EN as u8,
        0,
        "dlfw_fail clears FWDL_EN"
    );

    // A firmware that never reports ready, with the key of a wrong image.
    let mut d = dev();
    d.hci.force.retain(|f| f.0 != MCUFW_CTRL);
    d.hci.regs[FW_DBG7 as usize..FW_DBG7 as usize + 4]
        .copy_from_slice(&ILLEGAL_KEY_GROUP.to_le_bytes());
    assert_eq!(
        download_firmware(&mut d, &fw),
        Err(Error::FirmwareDownload("invalid fw key"))
    );

    // The LTE coex window never ready.
    let mut d = dev();
    d.hci.force.retain(|f| f.0 != 0x1700);
    assert!(matches!(
        download_firmware(&mut d, &fw),
        Err(Error::Timeout {
            step: "ltecoex read",
            ..
        })
    ));
}

#[test]
fn flush_waits_for_pages_then_gives_up() {
    let mut d = dev();
    flush_queues(&mut d, false);
    assert_eq!(d.hci.delayed_us, 0, "rsvd == avail: nothing to wait for");
    d.hci.regs[0x0230] = 5;
    flush_queues(&mut d, false);
    assert_eq!(d.hci.delayed_us, 5 * 20_000);
    assert!(d.hci.log.iter().any(|a| matches!(
        a,
        Access::Read {
            width: 2,
            addr: 0x0232,
            ..
        }
    )));
}
