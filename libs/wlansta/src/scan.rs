//! Scanning and selection: the scan job, parsing results into the table,
//! which profiles a join may pick, and starting the join of the best BSS.

use alloc::vec;
use alloc::vec::Vec;

use ieee80211::{Bss, Mgmt};

use crate::machine::{Phase, ScanJob, Station};
use crate::select::{self, Candidate, Join, Pick};
use crate::types::*;
use crate::{AUTO_SCAN_MS, FRESH_MS, MAX_SCAN_SSIDS, PMK_TIMEOUT_MS, SCAN_MAX_AGE_MS, SCAN_TIMEOUT_MS};

impl<N: FnMut() -> [u8; 32] + Clone> Station<N> {
    pub(crate) fn start_scan(&mut self, now: u64, join: bool, ssids: Vec<Vec<u8>>) -> Vec<Action> {
        if let Some(job) = self.scan.as_mut() {
            job.join |= join;
            return Vec::new();
        }
        self.scan = Some(ScanJob {
            id: None,
            deadline: now + SCAN_TIMEOUT_MS,
            join,
        });
        vec![Action::Scan { ssids }]
    }

    pub(crate) fn scan_done(&mut self, now: u64, scan_id: u32, entries: &[ScanEntry]) -> Vec<Action> {
        if self.scan.as_ref().and_then(|job| job.id) != Some(scan_id) {
            return Vec::new();
        }
        let job = self.scan.take();
        self.ingest(now, entries);
        if job.is_some_and(|job| job.join) && matches!(self.phase, Phase::Scanning) {
            return self.select_and_join(now, true);
        }
        Vec::new()
    }

    /// Parse every entry into the table (hostile frames are dropped by
    /// `ieee80211`), age the table, and count a scan generation.
    pub(crate) fn ingest(&mut self, now: u64, entries: &[ScanEntry]) {
        for entry in entries {
            let (Some(band), Ok(channel)) = (Band::from_wire(entry.band), u8::try_from(entry.channel))
            else {
                self.dropped += 1;
                continue;
            };
            let rssi = entry.rssi_dbm.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8;
            let seen = now.saturating_sub(u64::from(entry.age_ms));
            let bss = Mgmt::parse(&entry.frame)
                .ok()
                .and_then(|frame| Bss::from_frame(&frame, channel, rssi, seen));
            match bss {
                Some(bss) => {
                    self.bands.insert(bss.bssid, band);
                    self.table.update(bss);
                }
                None => self.dropped += 1,
            }
        }
        self.table.expire(now, SCAN_MAX_AGE_MS);
        let table = &self.table;
        self.bands.retain(|bssid, _| table.get(bssid).is_some());
        self.generation = self.generation.wrapping_add(1);
    }

    /// The profiles a join may pick now: the target, or auto-connect's.
    pub(crate) fn wanted(&self) -> Vec<&Profile> {
        match &self.target {
            Some((owner, id)) => self.profiles.iter().filter(|p| p.is(*owner, id)).collect(),
            None if self.suspended => Vec::new(),
            None => self
                .profiles
                .iter()
                .filter(|p| p.autoconnect && !self.memory.blocked(p.owner, &p.id))
                .filter(|p| match p.owner {
                    Owner::System => true,
                    Owner::User(uid) => self.active_users.contains(&uid),
                })
                .collect(),
        }
    }

    /// The best BSS for what is wanted; one that failed us lately is chosen
    /// only when nothing else fits (and says so, [`Candidate::penalized`]).
    pub(crate) fn pick_from_table(&self, now: u64, fresh_only: bool) -> Pick {
        let wanted = self.wanted();
        let heard = self.table.iter().filter_map(|bss| {
            let fresh = !fresh_only || now.saturating_sub(bss.last_seen) <= FRESH_MS;
            let band = *self.bands.get(&bss.bssid)?;
            fresh.then_some((bss, band))
        });
        select::pick(&wanted, heard, |bssid| self.memory.penalized(bssid, now))
    }

    /// Try to join: from the table when it is fresh (and the BSS has not
    /// failed us lately), else after a scan.
    pub(crate) fn attempt(&mut self, now: u64) -> Vec<Action> {
        self.auto_scan_at = None;
        if self.wanted().is_empty() {
            if matches!(self.phase, Phase::Failed { .. }) {
                self.phase = Phase::Failed { retry_at: None };
            } else {
                self.phase = Phase::Idle;
                self.report(ConnState::Idle, FailReason::None);
            }
            return Vec::new();
        }
        if let Pick::Found(cand) = self.pick_from_table(now, true) {
            if !cand.penalized {
                return self.begin(now, *cand);
            }
        }
        let probes: Vec<Vec<u8>> = self
            .wanted()
            .iter()
            .filter(|p| p.hidden)
            .take(MAX_SCAN_SSIDS)
            .map(|p| p.ssid.clone())
            .collect();
        self.phase = Phase::Scanning;
        self.report(ConnState::Scanning, FailReason::None);
        self.start_scan(now, true, probes)
    }

    /// After a join scan: join the best BSS (one that failed us lately too,
    /// when the scan found no other), or say why not.
    pub(crate) fn select_and_join(&mut self, now: u64, fresh_only: bool) -> Vec<Action> {
        match self.pick_from_table(now, fresh_only) {
            Pick::Found(cand) => self.begin(now, *cand),
            Pick::Mismatch if self.target.is_some() => {
                self.fail(now, FailReason::PolicyMismatch, None)
            }
            Pick::Unsupported if self.target.is_some() => {
                self.fail(now, FailReason::Unsupported, None)
            }
            Pick::NotFound if self.target.is_some() => self.fail(now, FailReason::NotFound, None),
            _ => {
                // Auto-connect found nothing it may join: look again later.
                self.phase = Phase::Idle;
                self.report(ConnState::Idle, FailReason::None);
                self.auto_scan_at = Some(now + AUTO_SCAN_MS);
                Vec::new()
            }
        }
    }

    /// Re-evaluate after the profiles or users changed: an idle station (not
    /// suspended) with something to join starts.
    pub(crate) fn reconsider(&mut self, now: u64) -> Vec<Action> {
        let idle = matches!(self.phase, Phase::Idle | Phase::Failed { retry_at: None });
        if idle && self.scan.is_none() && !self.wanted().is_empty() {
            return self.attempt(now);
        }
        Vec::new()
    }

    /// Start the join of `cand`: a WPA2 network needs its PMK first.
    pub(crate) fn begin(&mut self, now: u64, cand: Candidate) -> Vec<Action> {
        match cand.join {
            Join::Open => self.start_auth(now, cand, None),
            Join::Psk(_) => {
                let fetch = Action::FetchPmk {
                    id: cand.profile.id.clone(),
                    owner: cand.profile.owner,
                    ssid: cand.profile.ssid.clone(),
                };
                self.phase = Phase::WaitPmk {
                    cand,
                    deadline: now + PMK_TIMEOUT_MS,
                };
                self.report(ConnState::Authenticating, FailReason::None);
                vec![fetch]
            }
        }
    }
}
