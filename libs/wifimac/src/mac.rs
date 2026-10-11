//! [`SoftMac`]: the `hw.v1` control surface (owner, country, scan, join,
//! association state, keys, leave). Receive processing is `rx.rs`, transmit
//! `tx.rs`.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use ieee80211::frame::{Body, Mgmt};
use ieee80211::ie::{Elements, WPA_OUI};
use ieee80211::Mac;

use crate::events::{Event, Events, Stats};
use crate::keys::Keys;
use crate::radio::{Channel, Radio, RadioError, RadioInfo, StaState, CIPHER_CCMP128};
use crate::regdom::Domain;
use crate::scan::{self, Scanner};
use crate::Error;

/// Beacon intervals without a beacon from the joined BSS before
/// `BeaconLoss`, and the floor of that wait.
pub const LOSS_BEACONS: u64 = 10;
pub const LOSS_MIN_MS: u64 = 1000;
/// The beacon interval assumed until one is heard (100 TU).
pub(crate) const DEFAULT_INTERVAL_MS: u64 = 102;
/// Ethernet frames waiting for the network stack.
pub(crate) const RX_QUEUE: usize = 256;
/// The `Leave` reason used when the radio leaves on its own (a detach):
/// "deauthenticated because sending STA is leaving".
pub(crate) const REASON_LEAVING: u16 = 3;
/// The largest MSDU payload a 1500-octet Ethernet MTU needs.
const ETH_MTU: usize = 1500;
/// MAC header (QoS), CCMP header and LLC/SNAP around a payload.
const DATA_OVERHEAD: usize = 26 + 8 + 8;

/// What `Info` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwFacts {
    pub mac: Mac,
    pub bands: u32,
    pub ciphers: u32,
    pub scan_offload: bool,
    pub max_mgmt: u32,
    pub max_scan_ssids: u32,
    pub max_scan_channels: u32,
    pub key_slots: u32,
    pub features: u32,
    /// The Ethernet MTU the network card side carries.
    pub mtu: u16,
}

/// The joined BSS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bss {
    pub(crate) bssid: Mac,
    pub(crate) channel: Channel,
    pub(crate) aid: u16,
    /// The AP speaks QoS (HT or WMM in its beacon): data goes as QoS data.
    pub(crate) qos: bool,
    pub(crate) interval_ms: u64,
    pub(crate) last_beacon: u64,
    pub(crate) loss_reported: bool,
    /// A key was installed since the join: data must be protected from now
    /// on, both ways, even if the pairwise key is deleted again.
    pub(crate) protected: bool,
}

/// Sequence counters: one for management and non-QoS data, one per TID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sequences {
    plain: u16,
    qos: [u16; 16],
}

impl Sequences {
    pub(crate) fn next(&mut self, tid: Option<u8>) -> u16 {
        let counter = match tid {
            Some(tid) => &mut self.qos[usize::from(tid & 0x0F)],
            None => &mut self.plain,
        };
        let value = *counter;
        *counter = (*counter + 1) & 0x0FFF;
        value
    }
}

/// The soft-MAC over radio `R`.
pub struct SoftMac<R: Radio> {
    pub(crate) radio: R,
    pub(crate) info: RadioInfo,
    pub(crate) owner: Option<u64>,
    pub(crate) domain: Option<Domain>,
    pub(crate) scanner: Scanner,
    pub(crate) bss: Option<Bss>,
    pub(crate) state: StaState,
    pub(crate) keys: Keys,
    pub(crate) seq: Sequences,
    pub(crate) events: Events,
    pub(crate) rx_eth: VecDeque<Vec<u8>>,
    pub(crate) stats: Stats,
}

/// What a beacon or probe response says that the soft-MAC uses: QoS, and
/// the beacon interval in milliseconds.
pub(crate) fn bss_traits(frame: &[u8]) -> Option<(bool, u64)> {
    let mgmt = Mgmt::parse(frame).ok()?;
    let (Body::Beacon(b) | Body::ProbeResponse(b)) = mgmt.body else {
        return None;
    };
    let ies = Elements::parse(b.ies).ok()?;
    let wmm = ies.vendor.iter().any(|v| v.oui == WPA_OUI && v.kind == 2);
    let interval = (u64::from(b.interval) * 1024 / 1000).max(10);
    Some((ies.ht_cap.is_some() || wmm, interval))
}

impl<R: Radio> SoftMac<R> {
    /// Take over `radio` and program its address.
    pub fn new(mut radio: R) -> SoftMac<R> {
        let info = radio.info();
        let mut stats = Stats::default();
        if radio.set_address(info.mac).is_err() {
            stats.fw_errors += 1;
        }
        SoftMac {
            radio,
            info,
            owner: None,
            domain: None,
            scanner: Scanner::default(),
            bss: None,
            state: StaState::Idle,
            keys: Keys::default(),
            seq: Sequences::default(),
            events: Events::default(),
            rx_eth: VecDeque::new(),
            stats,
        }
    }

    pub fn radio(&self) -> &R {
        &self.radio
    }

    pub fn radio_mut(&mut self) -> &mut R {
        &mut self.radio
    }

    /// The Ethernet MTU of the network-card side: what fits a QoS data frame
    /// with a CCMP header in the radio's largest frame, at most 1500.
    pub fn mtu(&self) -> u16 {
        let room = self.info.max_frame.saturating_sub(DATA_OVERHEAD);
        room.min(ETH_MTU) as u16
    }

    pub(crate) fn max_mgmt(&self) -> usize {
        self.info.max_frame.min(ieee80211::data::MAX_MSDU)
    }

    /// `Info`: open to anyone.
    pub fn info(&self) -> HwFacts {
        HwFacts {
            mac: self.info.mac,
            bands: self.info.bands,
            ciphers: self.info.ciphers & (1 << CIPHER_CCMP128),
            scan_offload: false,
            max_mgmt: self.max_mgmt() as u32,
            max_scan_ssids: scan::MAX_SCAN_SSIDS as u32,
            max_scan_channels: scan::MAX_SCAN_CHANNELS as u32,
            key_slots: self.info.key_slots,
            features: 0,
            mtu: self.mtu(),
        }
    }

    /// `Stats`: open to anyone (hardware counters only).
    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn owner(&self) -> Option<u64> {
        self.owner
    }

    /// The association state the client last set.
    pub fn state(&self) -> StaState {
        self.state
    }

    /// Whether the network card's link is up: authorized in a BSS.
    pub fn link_up(&self) -> bool {
        self.bss.is_some() && self.state == StaState::Authorized
    }

    pub(crate) fn owned(&self, caller: u64) -> Result<(), Error> {
        match self.owner {
            Some(owner) if owner == caller => Ok(()),
            _ => Err(Error::Denied),
        }
    }

    /// Count a failed radio operation and say what the caller hears.
    pub(crate) fn radio_failed(&mut self, error: RadioError) -> Error {
        match error {
            RadioError::Full => Error::NoSpace,
            RadioError::Invalid => Error::Invalid,
            RadioError::Hardware => {
                self.stats.fw_errors += 1;
                Error::Radio
            }
        }
    }

    /// Queue an event for the owner (nothing is queued without one).
    pub(crate) fn emit(&mut self, event: Event) {
        if self.owner.is_some() && !self.events.push(event) {
            self.stats.mgmt_dropped += 1;
        }
    }

    /// The next event for the owner.
    pub fn next_event(&mut self) -> Option<Event> {
        self.events.pop()
    }

    /// `Attach`: `caller` becomes the one client.
    pub fn attach(&mut self, caller: u64) -> Result<(), Error> {
        if self.owner.is_some() {
            return Err(Error::Busy);
        }
        self.owner = Some(caller);
        self.events.clear();
        Ok(())
    }

    /// `Detach` by the owner: everything [`SoftMac::release`] does.
    pub fn detach(&mut self, caller: u64) -> Result<(), Error> {
        self.owned(caller)?;
        self.release();
        Ok(())
    }

    /// The owner is gone (it exited, its event channel broke, or it
    /// detached): stop the scan, leave the BSS, delete every key, forget the
    /// country and the queued events.
    pub fn release(&mut self) {
        if self.scanner.running.is_some() {
            self.scanner.running = None;
            let _ = self.radio.set_scan_filter(false);
        }
        self.disconnect(Some(REASON_LEAVING));
        self.domain = None;
        self.owner = None;
        self.events.clear();
    }

    /// `SetCountry`.
    pub fn set_country(&mut self, caller: u64, code: &str) -> Result<(), Error> {
        self.owned(caller)?;
        self.domain = Some(Domain::parse(code).ok_or(Error::Invalid)?);
        Ok(())
    }

    /// Run pending work: received frames, the scan's next channel, the
    /// beacon-loss watch.
    pub fn poll(&mut self, now: u64) {
        for _ in 0..crate::rx::RX_BUDGET {
            let Some(frame) = self.radio.receive() else {
                break;
            };
            self.on_receive(frame, now);
        }
        if let Some(run) = &self.scanner.running {
            if run.index.is_none() || now >= run.until_ms {
                self.scan_step(now);
            }
        }
        self.watch_beacons(now);
    }

    fn loss_deadline(&self) -> Option<u64> {
        let bss = self.bss.as_ref()?;
        if bss.loss_reported || self.scanner.running.is_some() {
            return None;
        }
        let wait = (bss.interval_ms * LOSS_BEACONS).max(LOSS_MIN_MS);
        Some(bss.last_beacon + wait)
    }

    fn watch_beacons(&mut self, now: u64) {
        if self.loss_deadline().is_some_and(|deadline| now >= deadline) {
            if let Some(bss) = self.bss.as_mut() {
                bss.loss_reported = true;
            }
            self.stats.beacon_losses += 1;
            self.emit(Event::BeaconLoss);
        }
    }

    /// When [`SoftMac::poll`] next has timed work to do.
    pub fn next_deadline(&self) -> Option<u64> {
        let scan = self.scanner.running.as_ref().map(|run| run.until_ms);
        match (scan, self.loss_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}
