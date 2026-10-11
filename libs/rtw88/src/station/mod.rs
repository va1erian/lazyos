//! The station side of the driver: what a soft-MAC on top needs from the
//! chip (upstream `mac80211.c` ops and the `main.c` vif/sta handling they
//! call: `rtw_ops_add_interface`, `rtw_vif_port_config`,
//! `rtw_ops_configure_filter`, `rtw_ops_bss_info_changed`,
//! `rtw_sta_add`/`rtw_sta_remove`, `rtw_ops_set_key`,
//! `rtw_core_scan_start`/`complete`, `rtw_set_channel`).
//!
//! Upstream calls `rtw_leave_lps`/`rtw_leave_lps_deep` at the top of most
//! of these; they do nothing unless the chip is in power save, which this
//! driver never enters ([`crate::fw::leave_lps`] is there for a caller
//! that wants to insist).
//!
//! There is one interface, a station on hardware port 0 with MAC id 0 (what
//! upstream picks for the first vif). Everything is a method on
//! [`Dev`]; [`Vif`] is the state it keeps in `dev.hal.vif`.
//!
//! The order a station follows:
//!
//! 1. [`Dev::add_station`] with the address to use (usually
//!    [`Dev::mac_addr`]).
//! 2. Scanning: [`Dev::scan_start`], then per channel [`Dev::set_channel`]
//!    and [`Dev::tx_mgmt`] probe requests, received beacons and probe
//!    responses come up the RX ring; [`Dev::scan_complete`].
//! 3. Joining: [`Dev::set_channel`], [`Dev::prepare_tx`] (calibration),
//!    [`Dev::set_bssid`], authentication and association frames through
//!    [`Dev::tx_mgmt`], then [`Dev::assoc`] with the AP's capabilities.
//! 4. Keys after the 4-way handshake: [`Dev::set_key`] (pairwise and
//!    group); data through [`Dev::tx_data`].
//! 5. Leaving: [`Dev::del_key`], [`Dev::disassoc`], [`Dev::set_bssid`] with
//!    zeros.

pub mod keys;
pub mod ra;
pub mod txb;

#[cfg(test)]
mod tests;

pub use keys::Cipher;
pub use ra::{HtCaps, PeerCaps, StaInfo, VhtCaps};
pub use txb::TxFrame;

use crate::dev::{Band, Bw, Dev};
use crate::error::{Error, Result};
use crate::fw::{self, C2h, C2hEvent};
use crate::hci::{Hci, RegExt};
use crate::mac::regs::{
    BCN_CTRL, EN_BCN_FUNCTION, RCR, RCR_AAP, RCR_ACRC32, RCR_AM, RCR_CBSSID_BCN, RCR_CBSSID_DATA,
    RXFLTMAP1,
};

/// Upstream `enum rtw_net_type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum NetType {
    #[default]
    NoLink = 0,
    AdHoc = 1,
    MgdLinked = 2,
    ApMode = 3,
}

/// Port 0's registers (upstream `rtw_vif_port[0]`).
mod port0 {
    pub const MAC_ADDR: u32 = 0x0610;
    pub const BSSID: u32 = 0x0618;
    pub const NET_TYPE: (u32, u32) = (0x0100, 0x30000);
    pub const AID: (u32, u32) = (0x06a8, 0x7ff);
    pub const BCN_CTRL: (u32, u8) = (super::BCN_CTRL, 0xff);
}

/// What [`port_config`] writes (upstream `enum rtw_vif_port_set`).
pub mod port_set {
    pub const MAC_ADDR: u32 = 1 << 0;
    pub const BSSID: u32 = 1 << 1;
    pub const NET_TYPE: u32 = 1 << 2;
    pub const AID: u32 = 1 << 3;
    pub const BCN_CTRL: u32 = 1 << 4;
}

/// The station interface (upstream `struct rtw_vif`) and its AP.
#[derive(Clone, Debug, Default)]
pub struct Vif {
    /// [`Dev::add_station`] ran and [`Dev::remove_station`] did not.
    pub added: bool,
    pub net_type: NetType,
    pub aid: u16,
    pub mac_id: u8,
    pub port: u8,
    pub mac_addr: [u8; 6],
    pub bssid: [u8; 6],
    pub bcn_ctrl: u8,
    /// The BSS basic rates, a bitmap in the band's rate order (see
    /// [`PeerCaps::supp_rates`]); management frames go at the lowest one.
    pub basic_rates: u32,
    /// The AP while associated (upstream `struct rtw_sta_info`).
    pub sta: Option<StaInfo>,
    /// The firmware is filtering our BSS's beacons.
    pub beacon_filter: bool,
}

/// RX filter knobs (upstream `rtw_ops_configure_filter`'s `FIF_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxFilter {
    pub all_multi: bool,
    pub fcs_fail: bool,
    pub other_bss: bool,
    /// Beacons and probe responses of every BSS (scanning).
    pub bcn_prbresp_promisc: bool,
    pub control: bool,
}

/// Upstream `rtw_vif_port_config` for port 0.
pub fn port_config<H: Hci>(dev: &mut Dev<H>, config: u32) {
    let vif = dev.hal.vif.clone();
    let hci = &mut dev.hci;
    let write_addr = |hci: &mut H, start: u32, a: &[u8; 6]| {
        for (i, b) in a.iter().enumerate() {
            hci.write8(start + i as u32, *b);
        }
    };
    if config & port_set::MAC_ADDR != 0 {
        write_addr(hci, port0::MAC_ADDR, &vif.mac_addr);
    }
    if config & port_set::BSSID != 0 {
        write_addr(hci, port0::BSSID, &vif.bssid);
    }
    if config & port_set::NET_TYPE != 0 {
        hci.write32_mask(port0::NET_TYPE.0, port0::NET_TYPE.1, vif.net_type as u32);
    }
    if config & port_set::AID != 0 {
        hci.write32_mask(port0::AID.0, port0::AID.1, u32::from(vif.aid));
    }
    if config & port_set::BCN_CTRL != 0 {
        hci.write8_mask(port0::BCN_CTRL.0, port0::BCN_CTRL.1, vif.bcn_ctrl);
    }
}

impl<H: Hci> Dev<H> {
    /// The card's own address (efuse), valid after `chip_info_setup`.
    pub fn mac_addr(&self) -> [u8; 6] {
        self.efuse.addr
    }

    /// Upstream `rtw_ops_add_interface` for a station: MAC id 0 on port 0,
    /// not linked, beacon function (TSF) on.
    pub fn add_station(&mut self, mac: [u8; 6]) -> Result<()> {
        if self.hal.vif.added {
            return Err(Error::Invalid("station interface already added"));
        }
        self.hal.vif = Vif {
            added: true,
            mac_addr: mac,
            bcn_ctrl: EN_BCN_FUNCTION,
            ..Vif::default()
        };
        port_config(
            self,
            port_set::MAC_ADDR | port_set::NET_TYPE | port_set::BCN_CTRL,
        );
        Ok(())
    }

    /// Upstream `rtw_ops_remove_interface`.
    pub fn remove_station(&mut self) -> Result<()> {
        self.hal.vif = Vif::default();
        port_config(
            self,
            port_set::MAC_ADDR | port_set::NET_TYPE | port_set::BCN_CTRL,
        );
        Ok(())
    }

    /// Change the port's address (upstream does this for a scan with a
    /// random address, `rtw_core_scan_start`).
    pub fn set_port(&mut self, mac: [u8; 6]) {
        self.hal.vif.mac_addr = mac;
        port_config(self, port_set::MAC_ADDR);
    }

    /// Upstream `BSS_CHANGED_BSSID`: the BSS to receive from (zeros to
    /// forget it).
    pub fn set_bssid(&mut self, bssid: [u8; 6]) {
        self.hal.vif.bssid = bssid;
        port_config(self, port_set::BSSID);
    }

    /// Upstream `rtw_ops_configure_filter` with every flag "changed".
    /// Turning promiscuous beacons off sets `CBSSID_BCN` only, as upstream.
    pub fn set_rx_filter(&mut self, f: RxFilter) {
        let ctl = if f.control {
            0xffff
        } else {
            self.hal.rxfltmap1
        };
        self.hci.write16(RXFLTMAP1, ctl);
        let mut rcr = self.hal.rcr;
        let set =
            |rcr: &mut u32, on: bool, bits: u32| if on { *rcr |= bits } else { *rcr &= !bits };
        set(&mut rcr, f.all_multi, RCR_AM);
        set(&mut rcr, f.fcs_fail, RCR_ACRC32);
        set(&mut rcr, f.other_bss, RCR_AAP);
        if f.bcn_prbresp_promisc {
            rcr &= !(RCR_CBSSID_BCN | RCR_CBSSID_DATA);
        } else {
            rcr |= RCR_CBSSID_BCN;
        }
        self.hal.rcr = rcr;
        self.hci.write32(RCR, rcr);
    }

    /// Scanning (`true`: every BSS's beacons and probe responses) or
    /// associated (`false`: our BSS's beacons only).
    pub fn set_filter_scan(&mut self, scanning: bool) {
        self.set_rx_filter(RxFilter {
            bcn_prbresp_promisc: scanning,
            ..RxFilter::default()
        });
    }

    /// Upstream `rtw_core_scan_start` (software scan): leave power save, use
    /// `mac` on the port, tell the firmware, receive every BSS.
    pub fn scan_start(&mut self, mac: [u8; 6]) -> Result<()> {
        self.set_port(mac);
        if self.fw.has(fw::feature::NOTIFY_SCAN) {
            fw::scan_notify(self, true)?;
        }
        self.hal.flags.scanning = true;
        crate::phy::dig_set_max_coverage(self);
        self.set_filter_scan(true);
        Ok(())
    }

    /// Upstream `rtw_core_scan_complete`: back to the interface address and
    /// our BSS's beacons. The firmware answers the stop with
    /// `C2H_SCAN_RESULT`; upstream waits 10 ms for it, here it simply
    /// arrives on the RX ring.
    pub fn scan_complete(&mut self) -> Result<()> {
        crate::phy::dig_reset(self);
        self.hal.flags.scanning = false;
        if self.fw.has(fw::feature::NOTIFY_SCAN) {
            fw::scan_notify(self, false)?;
        }
        let mac = self.hal.vif.mac_addr;
        self.set_port(mac);
        self.set_filter_scan(false);
        Ok(())
    }

    /// Upstream `rtw_set_channel` + `rtw_update_channel`: tune to `primary`
    /// with width `bw` centred on `center` (equal to `primary` for 20 MHz).
    pub fn set_channel(&mut self, primary: u8, bw: Bw, center: u8) -> Result<()> {
        let idx = ra::update_channel(&mut self.hal, center, primary, bw)?;
        crate::rtw8822c::set_channel(self, center, bw, idx);
        crate::phy::set_tx_power_level(self, center);
        // Calibrating on every scan channel takes too long; upstream defers
        // it to the first transmission on an operating channel.
        if !self.hal.flags.scanning {
            self.hal.need_rfk = true;
        }
        Ok(())
    }

    /// Upstream `rtw_chip_prepare_tx` (`mgd_prepare_tx`): run the RF
    /// calibration [`Dev::set_channel`] deferred. Call before sending the
    /// authentication frame.
    pub fn prepare_tx(&mut self) {
        if self.hal.need_rfk {
            self.hal.need_rfk = false;
            crate::rtw8822c::phy_calibration(self);
        }
    }

    /// Associated with the BSS set by [`Dev::set_bssid`] as `aid`: upstream
    /// `BSS_CHANGED_ASSOC` (net type, AID, reserved pages, default port)
    /// and `rtw_sta_add` for the AP (rate adaptation, media status).
    pub fn assoc(&mut self, aid: u16, peer: &PeerCaps) -> Result<()> {
        if !self.hal.vif.added {
            return Err(Error::Invalid("no station interface"));
        }
        self.hal.vif.aid = aid;
        self.hal.vif.net_type = NetType::MgdLinked;
        self.hal.vif.basic_rates = peer.basic_rates;
        let (mac, bssid, mac_id, port) = (
            self.hal.vif.mac_addr,
            self.hal.vif.bssid,
            self.hal.vif.mac_id,
            self.hal.vif.port,
        );
        fw::rsvd::download_sta_rsvd_page(self, &mac, &bssid, aid)?;
        fw::default_port(self, port, mac_id)?;
        port_config(self, port_set::NET_TYPE | port_set::AID);

        let si = ra::update_sta_info(self, mac_id, peer);
        fw::send_ra_info(self, &si.ra_info(), true)?;
        fw::media_status_report(self, mac_id, true)?;
        let mut si = si;
        si.init_ra_lv = 0;
        self.hal.vif.sta = Some(si);
        self.set_filter_scan(false);
        Ok(())
    }

    /// Upstream `rtw_sta_remove` + `BSS_CHANGED_ASSOC` (left): the firmware
    /// forgets the AP, the port goes back to "no link".
    pub fn disassoc(&mut self) -> Result<()> {
        if self.hal.vif.beacon_filter {
            fw::beacon_filter_config(self, None)?;
            self.hal.vif.beacon_filter = false;
        }
        if let Some(si) = self.hal.vif.sta.take() {
            fw::media_status_report(self, si.mac_id, false)?;
        }
        self.hal.vif.aid = 0;
        self.hal.vif.net_type = NetType::NoLink;
        port_config(self, port_set::NET_TYPE | port_set::AID);
        Ok(())
    }

    /// Upstream `rtw_fw_beacon_filter_config` for the AP: hand beacon
    /// tracking to the firmware (it then reports `BcnFilter` C2H events and
    /// stops passing our BSS's beacons up). Off by default; only for
    /// firmware with `FW_FEATURE_BCN_FILTER`.
    pub fn set_beacon_filter(&mut self, enable: bool, beacon_int: u16) -> Result<()> {
        if !self.fw.has(fw::feature::BCN_FILTER) {
            return Err(Error::Unsupported("firmware has no beacon filter"));
        }
        let vif = &self.hal.vif;
        let cfg = fw::BcnFilter {
            bssid: vif.bssid,
            mac_id: vif.mac_id,
            beacon_int,
            thold: -70,
            hyst: 4,
        };
        fw::beacon_filter_config(self, enable.then_some(&cfg))?;
        self.hal.vif.beacon_filter = enable;
        Ok(())
    }

    /// Tell the firmware the AP's averaged signal (0-100, upstream
    /// `rtw_fw_send_rssi_info` from the watchdog) so its rate adaptation
    /// can use it.
    pub fn report_rssi(&mut self, rssi: u8) -> Result<()> {
        let Some(si) = self.hal.vif.sta else {
            return Ok(());
        };
        fw::send_rssi_info(self, si.mac_id, rssi.min(100), si.stbc_en != 0)
    }

    /// Decode a C2H packet from the RX ring and apply what concerns the
    /// station (a rate report updates [`StaInfo::tx_rate`]); the event is
    /// returned for the caller (TX reports, beacon filter news).
    pub fn handle_c2h<'a>(&mut self, pkt: &'a [u8]) -> Result<C2h<'a>> {
        let c2h = fw::parse_c2h(pkt)?;
        if let C2hEvent::RaReport {
            mac_id,
            rate,
            sgi,
            bw,
        } = c2h.event
        {
            if let Some(si) = self.hal.vif.sta.as_mut().filter(|si| si.mac_id == mac_id) {
                si.tx_rate = rate;
                si.tx_sgi = sgi;
                si.tx_bw = bw.unwrap_or(si.bw_mode as u8);
            }
        }
        Ok(c2h)
    }

    /// The band of the current channel.
    pub fn band(&self) -> Band {
        self.hal.current_band_type
    }
}
