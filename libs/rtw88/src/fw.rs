//! Firmware (upstream `fw.c`, `fw.h`): the image header, the download
//! (`rtw_download_firmware`, DDMA into the chip's IMEM/DMEM, the reserved
//! page path), H2C commands and C2H events.
//!
//! Owner: the core/MAC work package.
//!
//! Contract used by the chip work package: `send_h2c_cmd(dev, &[u8; 8])`
//! (upstream `rtw_fw_send_h2c_command`), and one function per H2C command
//! named after upstream (`do_iqk` for `rtw_fw_do_iqk`, ...).

/// What is known about the running firmware (upstream `struct rtw_fw_state`).
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
}
