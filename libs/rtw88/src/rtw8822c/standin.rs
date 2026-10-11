//! **STAND-INS** for functions other work packages own, so the chip code
//! builds and is tested before the merge. Each one is a faithful
//! translation of the named upstream function; at merge time replace the
//! body with a call into the owner's module (named on each item) and delete
//! the rest of this file.
//!
//! * [`send_h2c_cmd`] -> `crate::fw::send_h2c_cmd` (`rtw_fw_send_h2c_command`)
//! * [`send_h2c_packet`] -> the fw module's H2C packet path
//!   (`rtw_fw_send_h2c_packet`; here it only logs the packet into the H2C
//!   queue behind a zeroed descriptor, see the item)
//! * [`do_iqk`], [`inform_rfk_status`], [`query_bt_info`],
//!   [`coex_tdma_type`], [`bt_wifi_control`] -> the `crate::fw` helpers of
//!   the same upstream names
//! * [`set_channel_mac`] -> `crate::mac` (`rtw_set_channel_mac`)
//! * [`read8_physical_efuse`] -> `crate::efuse` (`rtw_read8_physical_efuse`)

use crate::dev::{Bw, Dev};
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::regs::*;

/// Upstream `H2C_PKT_SIZE`.
const H2C_PKT_SIZE: usize = 32;
const H2C_PKT_HDR_SIZE: u16 = 8;
const H2C_PKT_IQK: u16 = 0x0e;
const H2C_CMD_COEX_TDMA_TYPE: u8 = 0x60;
const H2C_CMD_QUERY_BT_INFO: u8 = 0x61;
const H2C_CMD_BT_WIFI_CONTROL: u8 = 0x69;
const H2C_CMD_WIFI_CALIBRATION: u8 = 0x6d;

/// Stand-in for `crate::fw::send_h2c_cmd` (upstream
/// `rtw_fw_send_h2c_command`): wait for the next mailbox to be free, write
/// the extension word then the command word, advance the box.
pub fn send_h2c_cmd<H: Hci>(dev: &mut Dev<H>, h2c: &[u8; 8]) {
    let bx = usize::from(dev.fw.last_box_num & 3);
    let mut waited = 0;
    while (dev.hci.read8(REG_HMETFR) >> bx) & 1 != 0 {
        if waited >= 3000 {
            return; // upstream: "failed to send h2c command"
        }
        dev.hci.delay_us(100);
        waited += 100;
    }
    let w0 = u32::from_le_bytes([h2c[0], h2c[1], h2c[2], h2c[3]]);
    let w1 = u32::from_le_bytes([h2c[4], h2c[5], h2c[6], h2c[7]]);
    dev.hci.write32(REG_HMEBOX_EX[bx], w1);
    dev.hci.write32(REG_HMEBOX[bx], w0);
    dev.fw.last_box_num = if dev.fw.last_box_num + 1 >= 4 { 0 } else { dev.fw.last_box_num + 1 };
}

/// Stand-in for the fw module's H2C packet path (`rtw_fw_send_h2c_packet`).
/// The real one fills the TX descriptor and the sequence number; this one
/// hands the 32 bytes to the bus behind a zeroed descriptor so tests can
/// read them back. It must not reach hardware.
pub fn send_h2c_packet<H: Hci>(dev: &mut Dev<H>, pkt: &[u8; H2C_PKT_SIZE]) {
    let mut buf = [0u8; crate::rtw8822c::spec::TX_PKT_DESC_SZ + H2C_PKT_SIZE];
    buf[crate::rtw8822c::spec::TX_PKT_DESC_SZ..].copy_from_slice(pkt);
    let _ = dev.hci.write_data_h2c(&buf);
}

/// Stand-in for `crate::fw::do_iqk` (upstream `rtw_fw_do_iqk`): the
/// firmware runs IQK when it gets H2C packet 0x0e with `clear` (bit 0) and
/// `segment_iqk` (bit 1) in the first payload byte.
pub fn do_iqk<H: Hci>(dev: &mut Dev<H>, clear: bool, segment_iqk: bool) {
    let mut pkt = [0u8; H2C_PKT_SIZE];
    // rtw_h2c_pkt_set_header: category 1, command id 0xff, sub-id.
    pkt[0] = 0x01;
    pkt[1] = 0xff;
    pkt[2..4].copy_from_slice(&H2C_PKT_IQK.to_le_bytes());
    pkt[4..6].copy_from_slice(&(H2C_PKT_HDR_SIZE + 1).to_le_bytes());
    pkt[8] = u8::from(clear) | (u8::from(segment_iqk) << 1);
    send_h2c_packet(dev, &pkt);
}

/// Stand-in for upstream `rtw_fw_inform_rfk_status` (H2C 0x6d, bit 8).
pub fn inform_rfk_status<H: Hci>(dev: &mut Dev<H>, start: bool) {
    send_h2c_cmd(dev, &[H2C_CMD_WIFI_CALIBRATION, u8::from(start), 0, 0, 0, 0, 0, 0]);
}

/// Stand-in for upstream `rtw_fw_query_bt_info` (H2C 0x61, bit 8).
pub fn query_bt_info<H: Hci>(dev: &mut Dev<H>) {
    send_h2c_cmd(dev, &[H2C_CMD_QUERY_BT_INFO, 1, 0, 0, 0, 0, 0, 0]);
}

/// Stand-in for upstream `rtw_fw_coex_tdma_type` (H2C 0x60).
pub fn coex_tdma_type<H: Hci>(dev: &mut Dev<H>, p: [u8; 5]) {
    send_h2c_cmd(dev, &[H2C_CMD_COEX_TDMA_TYPE, p[0], p[1], p[2], p[3], p[4], 0, 0]);
}

/// Stand-in for upstream `rtw_fw_bt_wifi_control` (H2C 0x69): an op code
/// and five data bytes.
pub fn bt_wifi_control<H: Hci>(dev: &mut Dev<H>, op: u8, data: [u8; 5]) {
    send_h2c_cmd(dev, &[H2C_CMD_BT_WIFI_CONTROL, op, data[0], data[1], data[2], data[3], data[4], 0]);
}

/// Stand-in for `crate::mac` (upstream `rtw_set_channel_mac`): the TX
/// sub-channel, the MAC's RF mode for the width, the 80 MHz MAC clock, and
/// CCK checking off in 2.4 GHz / on in 5 GHz.
pub fn set_channel_mac<H: Hci>(dev: &mut Dev<H>, channel: u8, bw: Bw, primary_ch_idx: u8) {
    let txsc20 = primary_ch_idx;
    let mut txsc40 = 0;
    if bw == Bw::Mhz80 {
        txsc40 = if txsc20 == RTW_SC_20_UPPER || txsc20 == RTW_SC_20_UPMOST {
            RTW_SC_40_UPPER
        } else {
            RTW_SC_40_LOWER
        };
    }
    dev.hci.write8(REG_DATA_SC, (txsc20 & 0xf) | ((txsc40 & 0xf) << 4));

    let mut v = dev.hci.read32(REG_WMAC_TRXPTCL_CTL) & !BIT_RFMOD;
    match bw {
        Bw::Mhz80 => v |= BIT_RFMOD_80M,
        Bw::Mhz40 => v |= BIT_RFMOD_40M,
        Bw::Mhz20 => {}
    }
    dev.hci.write32(REG_WMAC_TRXPTCL_CTL, v);

    // The 8822C's CPU is not an 8051: MAC clock at 80 MHz (MAC_CLK_HW_DEF_80M = 0).
    let v = dev.hci.read32(REG_AFE_CTRL1) & !BIT_MAC_CLK_SEL;
    dev.hci.write32(REG_AFE_CTRL1, v);
    dev.hci.write8(REG_USTIME_TSF, 80);
    dev.hci.write8(REG_USTIME_EDCA, 80);

    let mut v8 = dev.hci.read8(REG_CCK_CHECK) & !(BIT_CHECK_CCK_EN as u8);
    if crate::phy::ch_5g_band(channel).is_some() {
        v8 |= BIT_CHECK_CCK_EN as u8;
    }
    dev.hci.write8(REG_CCK_CHECK, v8);
}

/// Stand-in for `crate::efuse` (upstream `rtw_read8_physical_efuse`): one
/// byte of the physical efuse through `REG_EFUSE_CTRL`, `None` on timeout
/// (upstream's `EFUSE_READ_FAIL`).
pub fn read8_physical_efuse<H: Hci>(dev: &mut Dev<H>, addr: u16) -> Option<u8> {
    dev.hci.write32_mask(REG_EFUSE_CTRL, 0x3ff00, u32::from(addr));
    dev.hci.clr32(REG_EFUSE_CTRL, BIT_EF_FLAG);
    dev.hci
        .poll32("efuse read", REG_EFUSE_CTRL, BIT_EF_FLAG, BIT_EF_FLAG, 1000, 100_000)
        .ok()?;
    Some(dev.hci.read8(REG_EFUSE_CTRL))
}
