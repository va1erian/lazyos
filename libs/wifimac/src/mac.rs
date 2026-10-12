//! [`SoftMac`]: the `hw.v1` control surface (owner, country, events, the
//! timers). Association control is `assoc.rs`, scanning `scanning.rs`,
//! receive processing `rx.rs`, transmit `tx.rs`.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use ieee80211::Mac;

use crate::bss::{Bss, Sequences};
use crate::events::{Event, Events, Stats};
use crate::keys::Keys;
use crate::radio::{Channel, Radio, RadioError, RadioInfo, StaState, TxOptions, CIPHER_CCMP128};
use crate::regdom::{Domain, Use};
use crate::scan::{self, Scanner};
use crate::{Caller, Error};

/// Beacon intervals without a beacon from the joined BSS before
/// `BeaconLoss`, and the floor of that wait.
pub const LOSS_BEACONS: u64 = 10;
pub const LOSS_MIN_MS: u64 = 1000;
/// Ethernet frames waiting for the network stack.
pub(crate) const RX_QUEUE: usize = 256;
/// Frames from the owner waiting for the radio to be back on the BSS's
/// channel (a scan is stepping elsewhere) or allowed to transmit there.
pub(crate) const PENDING_TX: usize = 16;
/// `RxMgmt` events passed to the owner per second: a radio in range can
/// forge frames that look like the BSS's faster than anyone needs them.
pub const RXMGMT_PER_SEC: u32 = 32;
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

/// A one-second window counting `RxMgmt` events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Budget {
    start: u64,
    used: u32,
}

/// The soft-MAC over radio `R`.
pub struct SoftMac<R: Radio> {
    pub(crate) radio: R,
    pub(crate) info: RadioInfo,
    pub(crate) owner: Option<Caller>,
    pub(crate) domain: Option<Domain>,
    pub(crate) scanner: Scanner,
    pub(crate) bss: Option<Bss>,
    pub(crate) state: StaState,
    pub(crate) keys: Keys,
    pub(crate) seq: Sequences,
    pub(crate) events: Events,
    pub(crate) rx_eth: VecDeque<Vec<u8>>,
    pub(crate) stats: Stats,
    /// The channel the radio was last tuned to; `None` before the first tune
    /// or after one failed (where the radio is is then unknown).
    pub(crate) tuned: Option<Channel>,
    /// Owner frames built (sequenced, PN chosen) but held until the radio may
    /// transmit on the BSS's channel.
    pub(crate) pending: VecDeque<(Vec<u8>, TxOptions)>,
    /// The last poll took its whole receive budget: more may be waiting.
    pub(crate) rx_backlog: bool,
    pub(crate) mgmt_budget: Budget,
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
            tuned: None,
            pending: VecDeque::new(),
            rx_backlog: false,
            mgmt_budget: Budget::default(),
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

    pub fn owner(&self) -> Option<Caller> {
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

    /// The call is the owner's: the same task with the same credentials.
    pub(crate) fn owned(&self, caller: Caller) -> Result<(), Error> {
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

    /// Tune the radio, remembering where it is.
    pub(crate) fn tune(&mut self, channel: Channel) -> Result<(), RadioError> {
        let result = self.radio.tune(channel);
        self.tuned = result.is_ok().then_some(channel);
        result
    }

    /// Whether the radio may transmit to the joined BSS now: tuned to its
    /// channel (not away on a scan step), and on a channel the domain lists
    /// as passive only once the BSS has been heard there.
    pub(crate) fn tx_allowed(&self) -> bool {
        let (Some(bss), Some(domain)) = (self.bss.as_ref(), self.domain) else {
            return false;
        };
        let usable = match domain.allows(bss.channel.band, bss.channel.number) {
            Some(Use::Active) => true,
            Some(Use::Passive) => bss.heard,
            None => false,
        };
        usable && self.tuned == Some(bss.channel)
    }

    /// Queue an event for the owner (nothing is queued without one).
    /// Returns whether it is queued; what was lost is counted.
    pub(crate) fn emit(&mut self, event: Event) -> bool {
        if self.owner.is_none() {
            return false;
        }
        let pushed = self.events.push(event);
        self.stats.mgmt_dropped += pushed.dropped;
        pushed.queued
    }

    /// Whether one more `RxMgmt` fits this second's budget; counts it.
    pub(crate) fn admit_frame(&mut self, now: u64) -> bool {
        let budget = &mut self.mgmt_budget;
        if now >= budget.start.saturating_add(1000) || now < budget.start {
            *budget = Budget {
                start: now,
                used: 0,
            };
        }
        if budget.used >= RXMGMT_PER_SEC {
            self.stats.mgmt_dropped += 1;
            return false;
        }
        budget.used += 1;
        true
    }

    /// The next event for the owner.
    pub fn next_event(&mut self) -> Option<Event> {
        self.events.pop()
    }

    /// Give back an event [`SoftMac::next_event`] returned that could not be
    /// delivered (the owner's inbox was full): it is sent first next time.
    pub fn requeue_event(&mut self, event: Event) {
        if self.owner.is_some() {
            self.events.unpop(event);
        }
    }

    /// `Attach`: `caller` becomes the one client.
    pub fn attach(&mut self, caller: Caller) -> Result<(), Error> {
        if self.owner.is_some() {
            return Err(Error::Busy);
        }
        self.owner = Some(caller);
        self.events.clear();
        Ok(())
    }

    /// `Detach` by the owner: everything [`SoftMac::release`] does.
    pub fn detach(&mut self, caller: Caller) -> Result<(), Error> {
        self.owned(caller)?;
        self.release();
        Ok(())
    }

    /// The owner is gone (it exited, its event channel broke, or it
    /// detached): stop the scan, leave the BSS, delete every key, forget the
    /// country and the queued events.
    pub fn release(&mut self) {
        if self.scanner.running.take().is_some() {
            let now = self.bss.map_or(0, |bss| bss.last_beacon);
            self.end_scan_radio(now);
        }
        self.disconnect(Some(REASON_LEAVING));
        self.domain = None;
        self.owner = None;
        self.events.clear();
    }

    /// `SetCountry`. A running scan drops the channels the new domain does
    /// not allow and probes only where it allows it; a joined BSS on a
    /// channel the domain does not allow is left without a word on the air
    /// and reported as `Deauthenticated` (reason 3).
    pub fn set_country(&mut self, caller: Caller, code: &str) -> Result<(), Error> {
        self.owned(caller)?;
        let domain = Domain::parse(code).ok_or(Error::Invalid)?;
        self.domain = Some(domain);
        self.scanner.rebase(domain);
        let forbidden = self.bss.is_some_and(|bss| {
            domain
                .allows(bss.channel.band, bss.channel.number)
                .is_none()
        });
        if forbidden {
            self.disconnect(None);
            self.emit(Event::Deauthenticated {
                reason: REASON_LEAVING,
            });
        }
        Ok(())
    }

    /// Run pending work: received frames, the scan's next channel, the
    /// beacon-loss watch, frames held for the BSS's channel.
    pub fn poll(&mut self, now: u64) {
        let mut taken = 0;
        while taken < crate::rx::RX_BUDGET {
            let Some(frame) = self.radio.receive() else {
                break;
            };
            taken += 1;
            self.on_receive(frame, now);
        }
        self.rx_backlog = taken == crate::rx::RX_BUDGET;
        if let Some(run) = &self.scanner.running {
            if run.index.is_none() || now >= run.until_ms {
                self.scan_step(now);
            }
        }
        self.watch_beacons(now);
        self.flush_pending();
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
        if !self.loss_deadline().is_some_and(|deadline| now >= deadline) {
            return;
        }
        // Reported once per silence, and only once the owner has it queued:
        // a full queue retries at the next poll.
        if self.emit(Event::BeaconLoss) {
            if let Some(bss) = self.bss.as_mut() {
                bss.loss_reported = true;
            }
            self.stats.beacon_losses += 1;
        }
    }

    /// When [`SoftMac::poll`] next has timed work to do; 0 (due at once)
    /// when the last poll left received frames waiting.
    pub fn next_deadline(&self) -> Option<u64> {
        if self.rx_backlog {
            return Some(0);
        }
        let scan = self.scanner.running.as_ref().map(|run| run.until_ms);
        match (scan, self.loss_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}
