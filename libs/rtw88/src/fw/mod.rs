//! Firmware (upstream `fw.c`, `fw.h`): the image header ([`header`]), H2C
//! commands ([`h2c`] transport, [`cmds`] one function per command), C2H
//! events ([`c2h`]) and the reserved pages ([`rsvd`]). The download itself
//! is a MAC primitive, [`crate::mac::download_firmware`], as upstream keeps
//! it in `mac.c`.
//!
//! Owner: the core/MAC work package.
//!
//! Contract used by the chip work package: [`send_h2c_cmd`]`(dev, &[u8; 8])`
//! (upstream `rtw_fw_send_h2c_command`), and one function per H2C command
//! named after upstream ([`do_iqk`] for `rtw_fw_do_iqk`, ...).

pub mod c2h;
pub mod cmds;
pub mod h2c;
pub mod header;
pub mod rsvd;

#[cfg(test)]
mod tests;

pub use c2h::{c2h_isr, parse_c2h, C2h, C2hEvent};
pub use cmds::{
    beacon_filter_config, bt_ignore_wlan_action, bt_wifi_control, coex_tdma_type, default_port,
    do_iqk, force_bt_tx_power, inform_rfk_status, leave_lps, media_status_report, query_bt_info,
    scan_notify, send_general_info, send_phydm_info, send_ra_info, send_rssi_info,
    send_rsvd_page_h2c, set_pwr_mode, wl_ch_info, BcnFilter, IqkPara, PwrMode, RaInfo,
};
pub use h2c::{send_h2c_cmd, send_h2c_packet, send_h2c_register};
pub use header::{feature, Firmware, FwHeader, FwHeaderLegacy};

/// What is known about the running firmware (upstream `struct rtw_fw_state`
/// plus `rtwdev->h2c`).
#[derive(Clone, Debug, Default)]
pub struct FwState {
    pub version: u16,
    pub sub_version: u8,
    pub sub_index: u8,
    pub h2c_version: u16,
    pub feature: u32,
    pub feature_ext: u32,
    /// The next H2C box to use (upstream `rtwdev->h2c.last_box_num`).
    pub last_box_num: u8,
    /// The next H2C packet sequence number (upstream `rtwdev->h2c.seq`).
    pub seq: u16,
}

impl FwState {
    /// Upstream `__update_firmware_info`: remember what the image says.
    pub fn update(&mut self, fw: &Firmware<'_>) {
        self.h2c_version = fw.hdr.h2c_fmt_ver;
        self.version = fw.hdr.version;
        self.sub_version = fw.hdr.subversion;
        self.sub_index = fw.hdr.subindex;
        self.feature = fw.feature();
        self.feature_ext |= fw.feature_ext();
    }

    /// Upstream `rtw_fw_feature_check`.
    pub fn has(&self, feature: u32) -> bool {
        self.feature & feature != 0
    }
}
