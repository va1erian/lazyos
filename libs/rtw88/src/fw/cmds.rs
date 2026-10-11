//! The H2C commands a station needs (upstream `fw.c`: one function each,
//! same names without the `rtw_fw_` prefix; field layouts from `fw.h`).

use super::h2c::{send_h2c_cmd, send_h2c_packet, send_h2c_register, set_cmd_id, set_field, set_pkt_header, H2C_PKT_SIZE};
use crate::dev::Dev;
use crate::error::Result;
use crate::hci::Hci;

/// Command ids (upstream `H2C_CMD_*`).
pub mod id {
    pub const RSVD_PAGE: u8 = 0x00;
    pub const MEDIA_STATUS_RPT: u8 = 0x01;
    pub const SET_PWR_MODE: u8 = 0x20;
    pub const DEFAULT_PORT: u8 = 0x2c;
    pub const RA_INFO: u8 = 0x40;
    pub const RSSI_MONITOR: u8 = 0x42;
    pub const BCN_FILTER_OFFLOAD_P0: u8 = 0x56;
    pub const BCN_FILTER_OFFLOAD_P1: u8 = 0x57;
    pub const SCAN: u8 = 0x59;
    pub const COEX_TDMA_TYPE: u8 = 0x60;
    pub const QUERY_BT_INFO: u8 = 0x61;
    pub const FORCE_BT_TX_POWER: u8 = 0x62;
    pub const IGNORE_WLAN_ACTION: u8 = 0x63;
    pub const WL_CH_INFO: u8 = 0x66;
    pub const BT_WIFI_CONTROL: u8 = 0x69;
    pub const WIFI_CALIBRATION: u8 = 0x6d;
}

/// Packet H2C sub-ids (upstream `H2C_PKT_*`).
pub mod pkt_id {
    pub const GENERAL_INFO: u8 = 0x0D;
    pub const IQK: u8 = 0x0E;
    pub const PHYDM_INFO: u8 = 0x11;
}

/// Upstream `enum rtw_fw_rf_type`, the two values this chip has.
const FW_RF_2T2R: u32 = 2;
const FW_RF_1T1R: u32 = 4;

/// Upstream `rtw_fw_send_general_info`: tell the firmware where its TX
/// buffer starts relative to the reserved boundary.
pub fn send_general_info<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    let mut pkt = [0u8; H2C_PKT_SIZE];
    set_pkt_header(&mut pkt, pkt_id::GENERAL_INFO, 4);
    let fifo = &dev.hal.fifo;
    let boundary = fifo.rsvd_fw_txbuf_addr.wrapping_sub(fifo.rsvd_boundary);
    set_field(&mut pkt, 2, 0x00FF_0000, u32::from(boundary));
    send_h2c_packet(dev, &mut pkt)
}

/// Upstream `rtw_fw_send_phydm_info`: RFE type, RF type, cut and antennas.
/// Upstream puts `antenna_tx` in the RX field and `antenna_rx` in the TX
/// field; kept as is (both are the same on this chip).
pub fn send_phydm_info<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    let mut pkt = [0u8; H2C_PKT_SIZE];
    let fw_rf_type = match dev.hal.rf_type {
        crate::dev::RF_1T1R => FW_RF_1T1R,
        crate::dev::RF_2T2R => FW_RF_2T2R,
        _ => 0,
    };
    set_pkt_header(&mut pkt, pkt_id::PHYDM_INFO, 8);
    set_field(&mut pkt, 2, 0x0000_00FF, u32::from(dev.efuse.rfe_option));
    set_field(&mut pkt, 2, 0x0000_FF00, fw_rf_type);
    set_field(&mut pkt, 2, 0x00FF_0000, u32::from(dev.hal.cut_version));
    set_field(&mut pkt, 2, 0x0F00_0000, dev.hal.antenna_tx);
    set_field(&mut pkt, 2, 0xF000_0000, dev.hal.antenna_rx);
    send_h2c_packet(dev, &mut pkt)
}

/// Upstream `struct rtw_iqk_para`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IqkPara {
    pub clear: u8,
    pub segment_iqk: u8,
}

/// Upstream `rtw_fw_do_iqk`: ask the firmware to run IQ calibration.
pub fn do_iqk<H: Hci>(dev: &mut Dev<H>, para: &IqkPara) -> Result<()> {
    let mut pkt = [0u8; H2C_PKT_SIZE];
    set_pkt_header(&mut pkt, pkt_id::IQK, 1);
    set_field(&mut pkt, 2, 1 << 0, u32::from(para.clear));
    set_field(&mut pkt, 2, 1 << 1, u32::from(para.segment_iqk));
    send_h2c_packet(dev, &mut pkt)
}

/// Upstream `rtw_fw_inform_rfk_status`.
pub fn inform_rfk_status<H: Hci>(dev: &mut Dev<H>, start: bool) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::WIFI_CALIBRATION);
    set_field(&mut h2c, 0, 1 << 8, u32::from(start));
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_media_status_report`: a peer (`mac_id`) connected or left.
pub fn media_status_report<H: Hci>(dev: &mut Dev<H>, mac_id: u8, connect: bool) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::MEDIA_STATUS_RPT);
    set_field(&mut h2c, 0, 1 << 8, u32::from(connect));
    set_field(&mut h2c, 0, 0x00FF_0000, u32::from(mac_id));
    send_h2c_cmd(dev, &h2c)
}

/// What the rate-adaptation H2C carries (upstream fields of
/// `struct rtw_sta_info` read by `rtw_fw_send_ra_info`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RaInfo {
    pub mac_id: u8,
    pub rate_id: u8,
    pub init_ra_lv: u8,
    pub sgi_enable: bool,
    pub bw_mode: u8,
    pub ldpc_en: u8,
    pub vht_enable: bool,
    pub ra_mask: u64,
}

/// Upstream `rtw_fw_send_ra_info` (the 8822C sends only the low 32 bits of
/// the mask: the `_HI` command is the 8814A's).
pub fn send_ra_info<H: Hci>(dev: &mut Dev<H>, si: &RaInfo, reset_ra_mask: bool) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::RA_INFO);
    set_field(&mut h2c, 0, 0x0000_FF00, u32::from(si.mac_id));
    set_field(&mut h2c, 0, 0x001F_0000, u32::from(si.rate_id));
    set_field(&mut h2c, 0, 0x0060_0000, u32::from(si.init_ra_lv));
    set_field(&mut h2c, 0, 1 << 23, u32::from(si.sgi_enable));
    set_field(&mut h2c, 0, 0x0300_0000, u32::from(si.bw_mode));
    set_field(&mut h2c, 0, 1 << 26, u32::from(si.ldpc_en != 0));
    set_field(&mut h2c, 0, 1 << 27, u32::from(!reset_ra_mask));
    set_field(&mut h2c, 0, 0x3000_0000, u32::from(si.vht_enable));
    // disable_pt: upstream always sets it.
    set_field(&mut h2c, 0, 1 << 30, 1);
    set_field(&mut h2c, 1, 0xFFFF_FFFF, si.ra_mask as u32);
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_send_rssi_info`: the averaged RSSI (0-100) of a peer.
pub fn send_rssi_info<H: Hci>(dev: &mut Dev<H>, mac_id: u8, rssi: u8, stbc: bool) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::RSSI_MONITOR);
    set_field(&mut h2c, 0, 0x0000_FF00, u32::from(mac_id));
    set_field(&mut h2c, 0, 0xFF00_0000, u32::from(rssi));
    set_field(&mut h2c, 1, 1 << 1, u32::from(stbc));
    send_h2c_cmd(dev, &h2c)
}

/// The power-save configuration (upstream `struct rtw_lps_conf`, the fields
/// `rtw_fw_set_pwr_mode` sends).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PwrMode {
    /// `enum rtw_lps_mode`: 0 active, 1 LPS, 2 WMM-PS.
    pub mode: u8,
    pub rlbm: u8,
    pub smart_ps: u8,
    pub awake_interval: u8,
    pub port_id: u8,
    /// `enum rtw_pwr_state`: 0 RF off, 4 RF on, 0xc all on.
    pub state: u8,
}

impl PwrMode {
    /// What `rtw_leave_lps_core` sends: active, everything on.
    pub const ACTIVE: PwrMode = PwrMode { mode: 0, rlbm: 0, smart_ps: 0, awake_interval: 1, port_id: 0, state: 0xc };
}

/// Upstream `rtw_fw_set_pwr_mode`.
pub fn set_pwr_mode<H: Hci>(dev: &mut Dev<H>, conf: &PwrMode) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::SET_PWR_MODE);
    set_field(&mut h2c, 0, 0x0000_7F00, u32::from(conf.mode));
    set_field(&mut h2c, 0, 0x000F_0000, u32::from(conf.rlbm));
    set_field(&mut h2c, 0, 0x00F0_0000, u32::from(conf.smart_ps));
    set_field(&mut h2c, 0, 0xFF00_0000, u32::from(conf.awake_interval));
    set_field(&mut h2c, 1, 0x0000_00E0, u32::from(conf.port_id));
    set_field(&mut h2c, 1, 0x0000_FF00, u32::from(conf.state));
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_leave_lps_core` without the LPS bookkeeping: the bus
/// leaves link power save and the firmware is told to stay awake. This
/// driver never enters LPS, so this is the only power-mode command it sends.
pub fn leave_lps<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    dev.hci.link_ps(false);
    set_pwr_mode(dev, &PwrMode::ACTIVE)
}

/// Upstream `rtw_fw_default_port`: the port whose TSF the firmware follows,
/// sent only once associated.
pub fn default_port<H: Hci>(dev: &mut Dev<H>, port: u8, mac_id: u8) -> Result<()> {
    let w0 = u32::from(id::DEFAULT_PORT) | (u32::from(port) << 8) | (u32::from(mac_id) << 16);
    send_h2c_register(dev, w0, 0)
}

/// Upstream `rtw_send_rsvd_page_h2c`: the page of each reserved packet.
pub fn send_rsvd_page_h2c<H: Hci>(dev: &mut Dev<H>, probe_resp: u8, ps_poll: u8, null: u8, qos_null: u8) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::RSVD_PAGE);
    h2c[1] = probe_resp;
    h2c[2] = ps_poll;
    h2c[3] = null;
    h2c[4] = qos_null;
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_wl_ch_info` (coex: the Wi-Fi channel).
pub fn wl_ch_info<H: Hci>(dev: &mut Dev<H>, link: u8, ch: u8, bw: u8) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::WL_CH_INFO);
    set_field(&mut h2c, 0, 0x0000_FF00, u32::from(link));
    set_field(&mut h2c, 0, 0x00FF_0000, u32::from(ch));
    set_field(&mut h2c, 0, 0xFF00_0000, u32::from(bw));
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_query_bt_info` (coex).
pub fn query_bt_info<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::QUERY_BT_INFO);
    set_field(&mut h2c, 0, 1 << 8, 1);
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_bt_ignore_wlan_action` (coex).
pub fn bt_ignore_wlan_action<H: Hci>(dev: &mut Dev<H>, enable: bool) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::IGNORE_WLAN_ACTION);
    set_field(&mut h2c, 0, 1 << 8, u32::from(enable));
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_force_bt_tx_power` (coex).
pub fn force_bt_tx_power<H: Hci>(dev: &mut Dev<H>, bt_pwr_dec_lvl: u8) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::FORCE_BT_TX_POWER);
    set_field(&mut h2c, 0, 0x0000_FF00, u32::from(0u8.wrapping_sub(bt_pwr_dec_lvl)));
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_coex_tdma_type` (coex).
pub fn coex_tdma_type<H: Hci>(dev: &mut Dev<H>, para: [u8; 5]) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::COEX_TDMA_TYPE);
    set_field(&mut h2c, 0, 0x0000_FF00, u32::from(para[0]));
    set_field(&mut h2c, 0, 0x00FF_0000, u32::from(para[1]));
    set_field(&mut h2c, 0, 0xFF00_0000, u32::from(para[2]));
    set_field(&mut h2c, 1, 0x0000_00FF, u32::from(para[3]));
    set_field(&mut h2c, 1, 0x0000_FF00, u32::from(para[4]));
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_bt_wifi_control` (coex).
pub fn bt_wifi_control<H: Hci>(dev: &mut Dev<H>, op_code: u8, data: [u8; 5]) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::BT_WIFI_CONTROL);
    set_field(&mut h2c, 0, 0x0000_FF00, u32::from(op_code));
    set_field(&mut h2c, 0, 0x00FF_0000, u32::from(data[0]));
    set_field(&mut h2c, 0, 0xFF00_0000, u32::from(data[1]));
    set_field(&mut h2c, 1, 0x0000_00FF, u32::from(data[2]));
    set_field(&mut h2c, 1, 0x0000_FF00, u32::from(data[3]));
    set_field(&mut h2c, 1, 0x00FF_0000, u32::from(data[4]));
    send_h2c_cmd(dev, &h2c)
}

/// Upstream `rtw_fw_scan_notify` (sent only when the firmware has
/// `FW_FEATURE_NOTIFY_SCAN`; it answers a stop with `C2H_SCAN_RESULT`).
pub fn scan_notify<H: Hci>(dev: &mut Dev<H>, start: bool) -> Result<()> {
    let mut h2c = [0u8; 8];
    set_cmd_id(&mut h2c, id::SCAN);
    set_field(&mut h2c, 0, 1 << 8, u32::from(start));
    send_h2c_cmd(dev, &h2c)
}

/// The beacon filter offload settings (upstream `rtw_fw_beacon_filter_config`
/// reads them from the BSS configuration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BcnFilter {
    pub bssid: [u8; 6],
    pub mac_id: u8,
    pub beacon_int: u16,
    /// CQM RSSI threshold in dBm (upstream default -70).
    pub thold: i32,
    /// CQM hysteresis in dB (upstream default 4).
    pub hyst: u32,
}

/// Upstream `rtw_fw_beacon_filter_config`: with `Some`, the firmware drops
/// beacons from our BSS and reports signal changes and connection loss by
/// C2H; `None` turns it off. Only for firmware with `FW_FEATURE_BCN_FILTER`.
pub fn beacon_filter_config<H: Hci>(dev: &mut Dev<H>, cfg: Option<&BcnFilter>) -> Result<()> {
    const BCN_LOSS_CNT: u32 = 10;
    let mut h2c = [0u8; 8];
    let Some(cfg) = cfg else {
        set_cmd_id(&mut h2c, id::BCN_FILTER_OFFLOAD_P1);
        return send_h2c_cmd(dev, &h2c);
    };
    set_cmd_id(&mut h2c, id::BCN_FILTER_OFFLOAD_P0);
    h2c[1..7].copy_from_slice(&cfg.bssid);
    send_h2c_cmd(dev, &h2c)?;

    let mut h2c = [0u8; 8];
    let thold = (cfg.thold + 100).clamp(0, 100) as u32;
    set_cmd_id(&mut h2c, id::BCN_FILTER_OFFLOAD_P1);
    set_field(&mut h2c, 0, 1 << 16, 1);
    // offload mode: BCN_FILTER_OFFLOAD_MODE_DEFAULT (0)
    set_field(&mut h2c, 0, 0x00E0_0000, 0);
    set_field(&mut h2c, 0, 0xFF00_0000, thold);
    set_field(&mut h2c, 1, 0x0000_000F, BCN_LOSS_CNT);
    set_field(&mut h2c, 0, 0x0000_FF00, u32::from(cfg.mac_id));
    set_field(&mut h2c, 0, 0x001E_0000, cfg.hyst);
    set_field(&mut h2c, 1, 0x0000_3FF0, u32::from(cfg.beacon_int));
    send_h2c_cmd(dev, &h2c)
}
