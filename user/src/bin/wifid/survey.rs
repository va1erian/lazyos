//! First-light RX evidence (docs/wifi-rtw88-plan.md R3/R4): after bring-up,
//! hop the 20 MHz channels with the receive filter open to every BSS and
//! count what arrives, so a boot on the box shows whether the radio hears
//! anything before the station stack is assembled.
//!
//! **What is read from the air.** Only the fixed header fields of a frame
//! (frame control, and the third address of a management frame, the BSSID),
//! at constant offsets after a length check; no information element is
//! parsed here (that is `wlanmd`'s job, over `libs/ieee80211`). The result
//! is evidence lines, nothing else depends on it.

use alloc::vec::Vec;

use super::trace;

/// 2.4 GHz 1-13, then the 5 GHz channels every regulatory domain allows to
/// be listened on (UNII-1 and UNII-3; the radio only receives here).
pub(super) const CHANNELS: &[u8] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 36, 40, 44, 48, 149, 153, 157, 161, 165,
];
/// Ticks (100 Hz) on each channel: two beacon intervals of 102.4 ms.
pub(super) const DWELL_TICKS: u64 = 25;
/// Ticks between two sweeps.
pub(super) const REST_TICKS: u64 = 6000;
/// Most distinct BSSes remembered per sweep.
const MAX_BSS: usize = 48;

/// One BSS heard during a sweep.
#[derive(Clone, Copy)]
struct Bss {
    bssid: [u8; 6],
    channel: u8,
    beacons: u32,
    /// Strongest RX power seen (dBm, path A).
    best_dbm: i8,
}

/// Counters for one channel visit and one sweep.
#[derive(Default)]
pub(super) struct Survey {
    pub(super) channel: u8,
    frames: u32,
    mgmt: u32,
    beacons: u32,
    data: u32,
    crc_err: u32,
    c2h: u32,
    bss: Vec<Bss>,
    pub(super) sweeps: u32,
}

/// Frame control type and subtype of an 802.11 frame, if it has one.
fn kind(frame: &[u8]) -> Option<(u8, u8)> {
    let fc0 = *frame.first()?;
    Some(((fc0 >> 2) & 0x3, fc0 >> 4))
}

impl Survey {
    /// Start counting on `channel`.
    pub(super) fn enter(&mut self, channel: u8) {
        self.channel = channel;
        self.frames = 0;
        self.mgmt = 0;
        self.beacons = 0;
        self.data = 0;
        self.crc_err = 0;
        self.c2h = 0;
    }

    /// A firmware C2H packet arrived on the RX ring.
    pub(super) fn c2h(&mut self) {
        self.c2h += 1;
    }

    /// One received 802.11 frame (FCS included), with its descriptor facts.
    pub(super) fn frame(&mut self, frame: &[u8], crc_err: bool, rx_dbm: i8) {
        self.frames += 1;
        if crc_err {
            self.crc_err += 1;
            return;
        }
        match kind(frame) {
            Some((0, subtype)) => {
                self.mgmt += 1;
                // Beacon (8) or probe response (5): addr3 is the BSSID.
                if (subtype == 8 || subtype == 5) && frame.len() >= 24 {
                    self.beacons += 1;
                    let mut bssid = [0u8; 6];
                    bssid.copy_from_slice(&frame[16..22]);
                    self.heard(bssid, rx_dbm);
                }
            }
            Some((2, _)) => self.data += 1,
            _ => {}
        }
    }

    fn heard(&mut self, bssid: [u8; 6], rx_dbm: i8) {
        let channel = self.channel;
        if let Some(b) = self
            .bss
            .iter_mut()
            .find(|b| b.bssid == bssid && b.channel == channel)
        {
            b.beacons += 1;
            b.best_dbm = b.best_dbm.max(rx_dbm);
        } else if self.bss.len() < MAX_BSS {
            self.bss.push(Bss {
                bssid,
                channel,
                beacons: 1,
                best_dbm: rx_dbm,
            });
        }
    }

    /// `WIFID:SURVEY:CH` for the channel just left.
    pub(super) fn report_channel(&self) {
        trace::event(
            "SURVEY:CH",
            &[
                ("ch", &self.channel),
                ("frames", &self.frames),
                ("mgmt", &self.mgmt),
                ("beacons", &self.beacons),
                ("data", &self.data),
                ("crc_err", &self.crc_err),
                ("c2h", &self.c2h),
            ],
        );
    }

    /// `WIFID:SURVEY:BSS` per BSS heard and `WIFID:SURVEY:DONE`; clears
    /// the BSS list for the next sweep.
    pub(super) fn report_sweep(&mut self) {
        self.sweeps += 1;
        for b in &self.bss {
            let bssid = trace::Mac(b.bssid);
            trace::event(
                "SURVEY:BSS",
                &[
                    ("bssid", &bssid),
                    ("ch", &b.channel),
                    ("beacons", &b.beacons),
                    ("dbm", &b.best_dbm),
                ],
            );
        }
        trace::event(
            "SURVEY:DONE",
            &[("sweep", &self.sweeps), ("bss", &self.bss.len())],
        );
        self.bss.clear();
    }
}
