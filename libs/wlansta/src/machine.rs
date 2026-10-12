//! The station: its state, the commands a front end gives it, time and the
//! status it reports. Scanning and selection are `scan.rs`, the join itself
//! (authentication, association, handshake) `join.rs`, failures and what
//! they leave behind `failure.rs` and `memory.rs`.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use eapol::{Key, Standard, Supplicant};
use ieee80211::ScanTable;

use crate::memory::Memory;
use crate::select::Candidate;
use crate::ssid;
use crate::types::*;
use crate::{profile, reason, MAX_SCAN_SSIDS, SCAN_CAPACITY};

/// A join under way: the BSS, the PMK when WPA2, and its retransmission.
pub(crate) struct Joining {
    pub cand: Candidate,
    pub pmk: Option<Key<32>>,
    pub tries: u8,
    pub deadline: u64,
    /// The RSN element sent in the association request (whole).
    pub rsn_ie: Option<Vec<u8>>,
}

/// Associated; the 4-way handshake runs, or ran (`Connected`).
pub(crate) struct Link<N: FnMut() -> [u8; 32]> {
    pub cand: Candidate,
    pub aid: u16,
    pub sup: Option<Supplicant<Standard<N>>>,
    pub deadline: u64,
    /// Message 2 went out: a failure from here on may be a wrong passphrase.
    pub msg2_sent: bool,
}

pub(crate) enum Phase<N: FnMut() -> [u8; 32]> {
    Idle,
    /// A scan for a join runs; selection follows its results.
    Scanning,
    /// Waiting for `keyd`'s PMK for this BSS, until `deadline`.
    WaitPmk {
        cand: Candidate,
        deadline: u64,
    },
    Auth(Joining),
    Assoc(Joining),
    Handshake(Link<N>),
    Connected(Link<N>),
    /// The last attempt failed; try again at `retry_at` (never, when `None`).
    Failed {
        retry_at: Option<u64>,
    },
}

pub(crate) struct ScanJob {
    pub id: Option<u32>,
    pub deadline: u64,
    /// Selection waits for this scan.
    pub join: bool,
}

/// One radio's station. `N` draws a fresh 32-octet SNonce (the kernel CSPRNG
/// in `wlanmd`); it is cloned into each association's supplicant.
pub struct Station<N: FnMut() -> [u8; 32] + Clone> {
    pub(crate) mac: Mac,
    pub(crate) nonce: N,
    pub(crate) status: Status,
    pub(crate) phase: Phase<N>,
    pub(crate) scan: Option<ScanJob>,
    pub(crate) table: ScanTable,
    pub(crate) bands: BTreeMap<Mac, Band>,
    pub(crate) generation: u32,
    pub(crate) profiles: Vec<Profile>,
    pub(crate) active_users: Vec<u32>,
    /// Blocks, strikes and BSSID penalties left by failures.
    pub(crate) memory: Memory,
    /// The profile the user asked for (owner and id); `None` means
    /// auto-connect.
    pub(crate) target: Option<(Owner, String)>,
    /// `Disconnect` was asked: no auto-connect until the next `Connect`.
    pub(crate) suspended: bool,
    pub(crate) failures: u32,
    /// When the link came up; the back-off resets once it has held
    /// [`crate::STABLE_MS`].
    pub(crate) connected_at: Option<u64>,
    pub(crate) auto_scan_at: Option<u64>,
    seq: u16,
    /// Frames refused (malformed, foreign, out of place) since the start.
    pub(crate) dropped: u64,
}

impl<N: FnMut() -> [u8; 32] + Clone> Station<N> {
    /// A station for the radio whose address is `mac`.
    pub fn new(mac: Mac, nonce: N) -> Station<N> {
        Station {
            mac,
            nonce,
            status: Status::idle(),
            phase: Phase::Idle,
            scan: None,
            table: ScanTable::new(SCAN_CAPACITY),
            bands: BTreeMap::new(),
            generation: 0,
            profiles: Vec::new(),
            active_users: Vec::new(),
            memory: Memory::default(),
            target: None,
            suspended: false,
            failures: 0,
            connected_at: None,
            auto_scan_at: None,
            seq: 0,
            dropped: 0,
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Scans finished since the start (the `scan` topic's generation).
    pub fn scan_generation(&self) -> u32 {
        self.generation
    }

    /// Frames refused since the start.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The profiles the station holds (after [`profile::normalize`]).
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// The internal phase, for tests and logs.
    pub fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "idle",
            Phase::Scanning => "scanning",
            Phase::WaitPmk { .. } => "wait-pmk",
            Phase::Auth(_) => "auth",
            Phase::Assoc(_) => "assoc",
            Phase::Handshake(_) => "handshake",
            Phase::Connected(_) => "connected",
            Phase::Failed { .. } => "failed",
        }
    }

    /// When [`Event::Tick`] is next due, if anything waits for time.
    pub fn next_deadline(&self) -> Option<u64> {
        let phase = match &self.phase {
            Phase::WaitPmk { deadline, .. } => Some(*deadline),
            Phase::Auth(j) | Phase::Assoc(j) => Some(j.deadline),
            Phase::Handshake(link) => Some(link.deadline),
            Phase::Failed { retry_at } => *retry_at,
            _ => None,
        };
        [
            phase,
            self.scan.as_ref().map(|s| s.deadline),
            self.auto_scan_at,
            self.memory.next_expiry(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The next 12-bit management sequence number.
    pub(crate) fn next_seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0x0FFF;
        self.seq
    }

    /// Begin: join the best known network in range, if auto-connect has one.
    pub fn start(&mut self, now: u64) -> Vec<Action> {
        self.attempt(now)
    }

    /// Process one event.
    pub fn input(&mut self, now: u64, event: Event) -> Vec<Action> {
        match event {
            Event::ScanStarted { scan_id } => {
                if let Some(job) = self.scan.as_mut().filter(|job| job.id.is_none()) {
                    job.id = Some(scan_id);
                    Vec::new()
                } else {
                    vec![Action::AbortScan { scan_id }]
                }
            }
            Event::ScanDone {
                scan_id,
                aborted: _,
                entries,
            } => self.scan_done(now, scan_id, &entries),
            Event::Rx {
                kind,
                rssi_dbm,
                frame,
            } => self.rx(now, kind, rssi_dbm, &frame),
            Event::BeaconLoss => self.link_lost(now, FailReason::BeaconLoss, reason::INACTIVITY),
            Event::Deauthenticated { reason } => self.ended(now, reason),
            Event::HwError { op, errno: _ } => self.hw_error(now, op),
            Event::Tick => self.tick(now),
        }
    }

    // ---- commands ----------------------------------------------------------

    /// A front end asks for a scan, probing for `ssids` (hidden networks).
    pub fn scan(&mut self, now: u64, ssids: Vec<Vec<u8>>) -> Result<Vec<Action>, Refused> {
        if ssids.len() > MAX_SCAN_SSIDS || !ssids.iter().all(|s| ssid::valid(s)) {
            return Err(Refused::Invalid);
        }
        let joining = matches!(
            self.phase,
            Phase::WaitPmk { .. } | Phase::Auth(_) | Phase::Assoc(_) | Phase::Handshake(_)
        );
        if self.scan.is_some() || joining {
            return Err(Refused::Busy);
        }
        Ok(self.start_scan(now, false, ssids))
    }

    /// Join `owner`'s profile `id`: leave the current network, scan if it was
    /// not heard lately, pick its best BSS. A wrong-passphrase block and the
    /// strikes towards one are lifted.
    pub fn connect(&mut self, now: u64, owner: Owner, id: &str) -> Result<Vec<Action>, Refused> {
        if !self.profiles.iter().any(|p| p.is(owner, id)) {
            return Err(Refused::NoProfile);
        }
        let current = self.current().is_some_and(|c| c.profile.is(owner, id));
        if current && matches!(self.phase, Phase::Connected(_)) {
            return Ok(Vec::new());
        }
        self.memory.unblock(owner, id);
        self.target = Some((owner, String::from(id)));
        self.suspended = false;
        self.failures = 0;
        let mut out = self.reset(reason::LEAVING);
        out.extend(self.attempt(now));
        Ok(out)
    }

    /// Leave and stay away until the next [`Station::connect`].
    pub fn disconnect(&mut self, _now: u64) -> Vec<Action> {
        self.target = None;
        self.suspended = true;
        self.failures = 0;
        self.auto_scan_at = None;
        let out = self.reset(reason::LEAVING);
        self.report(ConnState::Idle, FailReason::None);
        out
    }

    /// Whose network a `Disconnect` would leave: the profile joined or being
    /// joined, else the one asked for, else the one whose failure is shown.
    pub fn current_owner(&self) -> Option<Owner> {
        let failed = (self.status.state == ConnState::Failed).then_some(self.status.owner);
        self.current()
            .map(|c| c.profile.owner)
            .or(self.target.as_ref().map(|(owner, _)| *owner))
            .or(failed.flatten())
    }

    /// Replace the known profiles ([`profile::normalize`]d). Leaving a network
    /// whose profile is gone or changed; joining one when idle and
    /// auto-connect now has a candidate.
    pub fn set_profiles(&mut self, now: u64, profiles: Vec<Profile>) -> Vec<Action> {
        self.profiles = profile::normalize(profiles, profile::MAX_PER_OWNER);
        self.memory.retain(&self.profiles);
        let gone = |(owner, id): &(Owner, String)| !self.profiles.iter().any(|p| p.is(*owner, id));
        if self.target.as_ref().is_some_and(gone) {
            self.target = None;
        }
        let stale = self
            .current()
            .is_some_and(|c| !self.profiles.contains(&c.profile));
        let mut out = Vec::new();
        if stale {
            out = self.reset(reason::LEAVING);
            self.report(ConnState::Idle, FailReason::None);
        }
        out.extend(self.reconsider(now));
        out
    }

    /// The uids logged in now: their own profiles become auto-connect
    /// candidates beside the system's. A user network whose owner is no
    /// longer logged in is left (a user's network is theirs while they are
    /// logged in).
    pub fn set_active_users(&mut self, now: u64, mut users: Vec<u32>) -> Vec<Action> {
        users.sort_unstable();
        users.dedup();
        self.active_users = users;
        let away =
            |owner: Owner| matches!(owner, Owner::User(uid) if !self.active_users.contains(&uid));
        if self.target.as_ref().is_some_and(|(owner, _)| away(*owner)) {
            self.target = None;
        }
        let mut out = Vec::new();
        if self.current().is_some_and(|c| away(c.profile.owner)) {
            out = self.reset(reason::LEAVING);
            self.report(ConnState::Idle, FailReason::None);
        }
        out.extend(self.reconsider(now));
        out
    }

    /// `keyd`'s answer to [`Action::FetchPmk`] for `owner`'s profile `id`.
    pub fn pmk(
        &mut self,
        now: u64,
        owner: Owner,
        id: &str,
        pmk: Result<Key<32>, PmkError>,
    ) -> Vec<Action> {
        let waiting =
            matches!(&self.phase, Phase::WaitPmk { cand, .. } if cand.profile.is(owner, id));
        if !waiting {
            return Vec::new();
        }
        let Phase::WaitPmk { cand, deadline } = core::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return Vec::new();
        };
        match pmk {
            Ok(pmk) => self.start_auth(now, cand, Some(pmk)),
            Err(error) => {
                self.phase = Phase::WaitPmk { cand, deadline };
                let why = match error {
                    PmkError::Missing => FailReason::BadPassphrase,
                    PmkError::Unavailable => FailReason::Timeout,
                };
                self.fail(now, why, None)
            }
        }
    }

    // ---- time and reporting --------------------------------------------------

    fn tick(&mut self, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(job) = self.scan.as_ref().filter(|job| now >= job.deadline) {
            if let Some(scan_id) = job.id {
                out.push(Action::AbortScan { scan_id });
            }
            let join = job.join;
            self.scan = None;
            if join && matches!(self.phase, Phase::Scanning) {
                // What the table held before this scan may be old: only
                // what was heard lately is joined.
                out.extend(self.select_and_join(now, true));
            }
        }
        if self.auto_scan_at.is_some_and(|at| now >= at) {
            self.auto_scan_at = None;
            if matches!(self.phase, Phase::Idle) && self.scan.is_none() {
                out.extend(self.attempt(now));
            }
        }
        if self.memory.expire(now) {
            out.extend(self.reconsider(now));
        }
        if matches!(self.phase, Phase::Failed { retry_at: Some(at) } if now >= at) {
            out.extend(self.attempt(now));
        }
        out.extend(self.join_tick(now));
        out
    }

    /// The candidate of the join or link in progress.
    pub(crate) fn current(&self) -> Option<&Candidate> {
        match &self.phase {
            Phase::WaitPmk { cand, .. } => Some(cand),
            Phase::Auth(j) | Phase::Assoc(j) => Some(&j.cand),
            Phase::Handshake(l) | Phase::Connected(l) => Some(&l.cand),
            _ => None,
        }
    }

    /// The profile of the target, if one is set and still known.
    pub(crate) fn target_profile(&self) -> Option<&Profile> {
        let (owner, id) = self.target.as_ref()?;
        self.profiles.iter().find(|p| p.is(*owner, id))
    }

    /// Report the state of the current candidate (or none).
    pub(crate) fn report(&mut self, state: ConnState, why: FailReason) {
        let (name, owner, bssid, freq, rssi) = match self.current() {
            Some(c) => (
                ssid::display(&c.profile.ssid),
                Some((c.profile.owner, c.profile.id.clone())),
                Some(c.bss.bssid),
                c.band.freq_mhz(c.bss.channel),
                i32::from(c.bss.rssi),
            ),
            None => (String::new(), None, None, 0, 0),
        };
        self.set_status(state, why, name, owner, bssid, freq, rssi);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_status(
        &mut self,
        state: ConnState,
        reason: FailReason,
        ssid: String,
        profile: Option<(Owner, String)>,
        bssid: Option<Mac>,
        freq_mhz: u32,
        rssi_dbm: i32,
    ) {
        let (owner, id) = match profile {
            Some((owner, id)) => (Some(owner), id),
            None => (None, String::new()),
        };
        let mut next = Status {
            state,
            ssid,
            bssid,
            rssi_dbm,
            freq_mhz,
            id,
            owner,
            reason,
            changes: self.status.changes,
        };
        if !next.same(&self.status) {
            next.changes = next.changes.wrapping_add(1);
        }
        self.status = next;
    }

    /// The connection succeeded: the profile's strikes go, and the back-off
    /// resets once the link has held.
    pub(crate) fn connected(&mut self, now: u64) {
        if let Some(c) = self.current() {
            let (owner, id) = (c.profile.owner, c.profile.id.clone());
            self.memory.clear_strikes(owner, &id);
        }
        self.connected_at = Some(now);
        self.report(ConnState::Connected, FailReason::None);
    }
}
