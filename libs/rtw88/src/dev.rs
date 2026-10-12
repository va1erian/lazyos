//! The device state (upstream `struct rtw_dev` and `struct rtw_hal`,
//! `main.h`).
//!
//! One [`Dev`] per chip. Each module owns the state it alone writes, as a
//! sub-struct defined in that module ([`crate::fw::FwState`],
//! [`crate::phy::PhyState`], ...), so modules can grow their state without
//! touching each other. [`Hal`] holds the facts many modules read: the chip
//! revision, the RF paths and the current channel.

use crate::coex::CoexState;
use crate::efuse::Efuse;
use crate::fw::FwState;
use crate::hci::Hci;
use crate::phy::PhyState;
use crate::rtw8822c::DmState;
use crate::sec::Cam;

/// Bands (upstream `enum rtw_supported_band` / `RTW_BAND_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Band {
    #[default]
    Ghz2,
    Ghz5,
}

/// Channel widths (upstream `enum rtw_bandwidth`, values are the hardware's).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Bw {
    #[default]
    Mhz20 = 0,
    Mhz40 = 1,
    Mhz80 = 2,
}

/// RF paths (upstream `enum rtw_rf_path`).
pub const RF_PATH_A: usize = 0;
pub const RF_PATH_B: usize = 1;
/// The RTL8822C has two paths.
pub const RF_PATH_MAX: usize = 2;

/// RF types (upstream `enum rtw_rf_type`, the two this chip comes in).
pub const RF_1T1R: u8 = 0;
pub const RF_2T2R: u8 = 2;

/// Baseband path bitmaps (upstream `enum rtw_bb_path`).
pub const BB_PATH_A: u32 = 1 << 0;
pub const BB_PATH_B: u32 = 1 << 1;
pub const BB_PATH_AB: u32 = BB_PATH_A | BB_PATH_B;

/// Facts many modules read (upstream `struct rtw_hal`, the part this driver
/// needs; tx power tables live in [`PhyState`]).
#[derive(Clone, Debug, Default)]
pub struct Hal {
    /// The receive configuration register value restored after power-on.
    pub rcr: u32,
    pub chip_version: u32,
    /// Chip cut (A = 0, B = 1, ...), from `REG_SYS_CFG1`.
    pub cut_version: u8,
    pub mp_chip: u8,
    pub oem_id: u8,
    pub pkg_type: u8,
    pub rf_type: u8,
    pub rf_path_num: u8,
    pub rf_phy_num: u8,
    pub antenna_tx: u32,
    pub antenna_rx: u32,
    pub current_channel: u8,
    pub current_primary_channel_index: u8,
    pub current_band_width: Bw,
    pub current_band_type: Band,
    pub primary_channel: u8,
    /// Centre channel for each width up to the current one.
    pub cch_by_bw: [u8; 3],

    /// `REG_RXFLTMAP1` as the chip's MAC init set it (control frames).
    pub rxfltmap1: u16,
    /// Calibrate before the next transmission (`rtwdev->need_rfk`).
    pub need_rfk: bool,
    /// The centre channel and width the RF was last calibrated on, so a
    /// re-tune to it sets no [`Hal::need_rfk`].
    pub rfk_channel: Option<(u8, Bw)>,
    /// Use one spatial stream only (`hal->txrx_1ss`).
    pub txrx_1ss: bool,
    /// The TX FIFO layout (`rtwdev->fifo`), written by `mac_init`.
    pub fifo: crate::mac::FifoConf,
    /// Device state flags (`rtwdev->flags`).
    pub flags: crate::mac::Flags,
    /// The one station interface and its peer (`struct rtw_vif` and
    /// `struct rtw_sta_info`), see [`crate::station`].
    pub vif: crate::station::Vif,
}

/// One RTL8822CE.
pub struct Dev<H: Hci> {
    pub hci: H,
    pub hal: Hal,
    pub efuse: Efuse,
    pub fw: FwState,
    pub phy: PhyState,
    pub dm: DmState,
    pub sec: Cam,
    pub coex: CoexState,
}

impl<H: Hci> Dev<H> {
    /// A device over `hci`, nothing read from it yet.
    pub fn new(hci: H) -> Dev<H> {
        Dev {
            hci,
            hal: Hal::default(),
            efuse: Efuse::default(),
            fw: FwState::default(),
            phy: PhyState::default(),
            dm: DmState::default(),
            sec: Cam::default(),
            coex: CoexState::default(),
        }
    }
}
