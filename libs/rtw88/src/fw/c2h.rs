//! Chip-to-host events (upstream `fw.c`: `rtw_fw_c2h_cmd_rx_irqsafe`,
//! `rtw_fw_c2h_cmd_handle`, `rtw_fw_c2h_cmd_handle_ext`,
//! `rtw_fw_ra_report_handle`, `rtw_fw_bcn_filter_notify`,
//! `rtw_fw_c2h_cmd_isr`; `tx.c` `rtw_tx_report_handle` for the CCX fields).
//!
//! A C2H arrives on the RX ring (the RX descriptor says so, see
//! [`crate::rx`]); what follows the descriptor and driver info is
//! `id, seq, payload...`. The firmware is trusted to run but not to be
//! well-formed: every event's length is checked before a field is read,
//! and a short event is an [`Error::Invalid`], never a panic.

use crate::error::{Error, Result};
use crate::hci::Hci;
use crate::mac::regs::{MCU_TST_CFG, VAL_FW_TRIGGER};
use crate::rtw8822c::spec::C2H_RA_REPORT_SIZE;

/// C2H ids (upstream `enum rtw_c2h_cmd_id`).
pub mod id {
    pub const CCX_TX_RPT: u8 = 0x03;
    pub const BT_INFO: u8 = 0x09;
    pub const BT_MP_INFO: u8 = 0x0b;
    pub const RA_RPT: u8 = 0x0c;
    pub const HW_FEATURE_REPORT: u8 = 0x19;
    pub const WLAN_INFO: u8 = 0x27;
    pub const WLAN_RFON: u8 = 0x32;
    pub const BCN_FILTER_NOTIFY: u8 = 0x36;
    pub const ADAPTIVITY: u8 = 0x37;
    pub const SCAN_RESULT: u8 = 0x38;
    pub const BT_HID_INFO: u8 = 0x45;
    pub const HW_FEATURE_DUMP: u8 = 0xfd;
    pub const HALMAC: u8 = 0xff;
}

/// Sub-ids of a `C2H_HALMAC` event (upstream `enum rtw_c2h_cmd_id_ext`).
pub mod ext_id {
    pub const SCAN_STATUS_RPT: u8 = 0x3;
    pub const CCX_RPT: u8 = 0x0f;
    pub const CHAN_SWITCH: u8 = 0x22;
}

/// A decoded event. Coexistence events carry their raw payload: LazyOS has
/// no Bluetooth stack, the coex code only needs to see them pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum C2hEvent<'a> {
    /// A TX report for a frame sent with `report` set: its sequence number
    /// (`TxPktInfo::sn`) and whether the peer acknowledged it.
    TxReport {
        sn: u8,
        acked: bool,
    },
    /// The firmware's rate adaptation picked a new rate for `mac_id`.
    RaReport {
        mac_id: u8,
        rate: u8,
        sgi: bool,
        bw: Option<u8>,
    },
    /// Beacon filter offload news (signal change, connection lost, ...).
    BcnFilter {
        kind: u8,
        high: bool,
        rssi: i8,
    },
    /// End of a firmware-assisted scan step, with the channel density.
    ScanResult {
        density: u8,
    },
    /// The RF is back on after leaving power save.
    WlanRfon,
    ScanStatus {
        code: u8,
    },
    ChanSwitch {
        central_ch: u8,
        id: u8,
        status: u8,
    },
    Coex {
        id: u8,
        payload: &'a [u8],
    },
    /// An event this driver does not act on.
    Other {
        id: u8,
        payload: &'a [u8],
    },
}

/// A C2H packet: its id, sequence number and decoded event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct C2h<'a> {
    pub id: u8,
    pub seq: u8,
    pub event: C2hEvent<'a>,
}

fn need(payload: &[u8], len: usize, what: &'static str) -> Result<()> {
    if payload.len() < len {
        Err(Error::Invalid(what))
    } else {
        Ok(())
    }
}

/// Decode the C2H packet `pkt` (starting at the id byte).
pub fn parse_c2h(pkt: &[u8]) -> Result<C2h<'_>> {
    if pkt.len() < 2 {
        return Err(Error::Invalid("c2h shorter than its header"));
    }
    let (c2h_id, seq, payload) = (pkt[0], pkt[1], &pkt[2..]);
    let event = match c2h_id {
        id::CCX_TX_RPT => {
            // GET_CCX_REPORT_SEQNUM_V0 / STATUS_V0
            need(payload, 7, "ccx tx report too short")?;
            C2hEvent::TxReport {
                sn: payload[6] & 0xfc,
                acked: payload[0] & 0xc0 == 0,
            }
        }
        id::RA_RPT => {
            // struct rtw_c2h_ra_rpt: rate_sgi, mac_id, ..., bw at 6.
            need(payload, C2H_RA_REPORT_SIZE, "ra report too short")?;
            C2hEvent::RaReport {
                mac_id: payload[1],
                rate: payload[0] & 0x7f,
                sgi: payload[0] & 0x80 != 0,
                bw: payload.get(6).copied(),
            }
        }
        id::BCN_FILTER_NOTIFY => {
            need(payload, 3, "beacon filter notify too short")?;
            C2hEvent::BcnFilter {
                kind: payload[1] & 0xf,
                high: payload[1] & 0x10 != 0,
                rssi: payload[2].wrapping_sub(100) as i8,
            }
        }
        id::SCAN_RESULT => {
            need(payload, 1, "scan result too short")?;
            C2hEvent::ScanResult {
                density: payload[0],
            }
        }
        id::WLAN_RFON => C2hEvent::WlanRfon,
        id::HALMAC => parse_halmac(payload)?,
        id::BT_INFO | id::BT_MP_INFO | id::BT_HID_INFO | id::WLAN_INFO => C2hEvent::Coex {
            id: c2h_id,
            payload,
        },
        _ => C2hEvent::Other {
            id: c2h_id,
            payload,
        },
    };
    Ok(C2h {
        id: c2h_id,
        seq,
        event,
    })
}

/// Upstream `rtw_fw_c2h_cmd_handle_ext`: the sub-id is the first payload byte.
fn parse_halmac(payload: &[u8]) -> Result<C2hEvent<'_>> {
    need(payload, 1, "halmac c2h without sub id")?;
    Ok(match payload[0] {
        ext_id::CCX_RPT => {
            // GET_CCX_REPORT_SEQNUM_V1 / STATUS_V1
            need(payload, 10, "ccx report too short")?;
            C2hEvent::TxReport {
                sn: payload[8] & 0xfc,
                acked: payload[9] & 0xc0 == 0,
            }
        }
        ext_id::SCAN_STATUS_RPT => {
            need(payload, 3, "scan status too short")?;
            C2hEvent::ScanStatus { code: payload[2] }
        }
        ext_id::CHAN_SWITCH => {
            need(payload, 5, "channel switch too short")?;
            C2hEvent::ChanSwitch {
                central_ch: payload[2],
                id: payload[3],
                status: payload[4],
            }
        }
        _ => C2hEvent::Other {
            id: id::HALMAC,
            payload,
        },
    })
}

/// Upstream `rtw_fw_c2h_cmd_isr`: the firmware raised its C2H interrupt;
/// `true` when it asks the driver for a firmware recovery.
pub fn c2h_isr<H: Hci>(hci: &mut H) -> bool {
    hci.read8(MCU_TST_CFG) == VAL_FW_TRIGGER
}
