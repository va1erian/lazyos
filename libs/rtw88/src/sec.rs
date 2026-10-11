//! The security CAM (upstream `sec.c`): hardware key slots, written with
//! `rtw_sec_write_cam`, cleared with `rtw_sec_clear_cam`, and the engine
//! switched on with `rtw_sec_enable_sec_engine`. CCMP is done by the chip.
//!
//! Owner: the core/MAC work package.

/// Which CAM entries are in use.
#[derive(Clone, Debug, Default)]
pub struct Cam {
    /// Bit *n* set: entry *n* holds a key.
    pub used: u64,
}
