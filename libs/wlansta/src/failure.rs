//! Failures: leaving, the reason reported, the back-off, and what a failure
//! teaches the [`crate::memory::Memory`] (a block, a strike, a BSSID
//! penalty).

use alloc::vec::Vec;

use crate::machine::{Phase, Station};
use crate::memory::STRIKES;
use crate::ssid;
use crate::types::*;
use crate::{reason, BACKOFF_MAX_MS, BACKOFF_MIN_MS, STABLE_MS};

/// Reasons an AP gives when it ends a 4-way handshake whose message 2 did
/// not verify (Table 9-49): 2 (previous authentication no longer valid), 15
/// (4-way handshake timeout), 23 (IEEE 802.1X authentication failed). After
/// our message 2 they are what a wrong passphrase looks like.
pub const BAD_PASSPHRASE_REASONS: [u16; 3] = [2, 15, 23];

impl<N: FnMut() -> [u8; 32] + Clone> Station<N> {
    pub(crate) fn hw_error(&mut self, now: u64, op: HwOp) -> Vec<Action> {
        match op {
            HwOp::Scan => {
                self.scan = None;
                if matches!(self.phase, Phase::Scanning) {
                    return self.fail(now, FailReason::Radio, None);
                }
                Vec::new()
            }
            HwOp::AbortScan | HwOp::Leave => Vec::new(),
            HwOp::Join | HwOp::Tx | HwOp::SetState | HwOp::SetKey => {
                if self.current().is_some() {
                    return self.fail(now, FailReason::Radio, Some(reason::UNSPECIFIED));
                }
                Vec::new()
            }
        }
    }

    /// Whether the radio is joined (it needs a `Leave` to be idle again).
    pub(crate) fn joined(&self) -> bool {
        matches!(
            self.phase,
            Phase::Auth(_) | Phase::Assoc(_) | Phase::Handshake(_) | Phase::Connected(_)
        )
    }

    /// Leave whatever is joined and drop a join scan; the phase becomes idle.
    pub(crate) fn reset(&mut self, leave_reason: u16) -> Vec<Action> {
        let mut out = Vec::new();
        if self.joined() {
            out.push(Action::Leave {
                reason: leave_reason,
            });
        }
        if let Some(job) = self.scan.as_mut() {
            job.join = false;
        }
        self.phase = Phase::Idle;
        self.connected_at = None;
        out
    }

    /// The attempt failed: leave, report why, remember what it says about
    /// the profile or the BSS, and schedule the retry.
    pub(crate) fn fail(&mut self, now: u64, why: FailReason, leave: Option<u16>) -> Vec<Action> {
        let failed = self
            .current()
            .map(|c| c.profile.clone())
            .or_else(|| self.target_profile().cloned());
        // Only a BSS we talked to can have failed us (not `keyd`, not a scan).
        let bssid = self
            .current()
            .map(|c| c.bss.bssid)
            .filter(|_| self.joined());
        let stable = self
            .connected_at
            .is_some_and(|at| now.saturating_sub(at) >= STABLE_MS);
        let out = self.reset(leave.unwrap_or(reason::UNSPECIFIED));
        if matches!(
            why,
            FailReason::Rejected | FailReason::Timeout | FailReason::BeaconLoss
        ) {
            if let Some(bssid) = bssid {
                self.memory.penalize(bssid, now);
            }
        }
        if let (FailReason::BadPassphrase, Some(profile)) = (why, &failed) {
            self.memory.block(profile.owner, &profile.id, now);
            if self.target.as_ref().is_some_and(|(o, i)| profile.is(*o, i)) {
                self.target = None;
            }
        }
        if stable {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        let retry = !self.wanted().is_empty();
        let shift = self.failures.saturating_sub(1).min(16);
        let delay = (BACKOFF_MIN_MS << shift).min(BACKOFF_MAX_MS);
        self.phase = Phase::Failed {
            retry_at: retry.then_some(now + delay),
        };
        let name = failed
            .as_ref()
            .map(|p| ssid::display(&p.ssid))
            .unwrap_or_default();
        let profile = failed.map(|p| (p.owner, p.id));
        self.set_status(ConnState::Failed, why, name, profile, None, 0, 0);
        out
    }

    /// A handshake that sent message 2 ended without a verified message 3.
    /// That is a wrong passphrase when the AP said so (`strong`: one of
    /// [`BAD_PASSPHRASE_REASONS`]) or when it is the profile's
    /// [`STRIKES`]th such end in a row; otherwise it is reported as
    /// `otherwise` and the BSS is avoided for a while, since a lost frame, a
    /// forged deauthentication or a rogue BSS that answers and then goes
    /// silent all look the same.
    pub(crate) fn handshake_failed(
        &mut self,
        now: u64,
        strong: bool,
        otherwise: FailReason,
        leave: u16,
    ) -> Vec<Action> {
        let Some((owner, id)) = self
            .current()
            .map(|c| (c.profile.owner, c.profile.id.clone()))
        else {
            return Vec::new();
        };
        let strikes = self.memory.strike(owner, &id, now);
        let why = if strong || strikes >= STRIKES {
            FailReason::BadPassphrase
        } else {
            otherwise
        };
        self.fail(now, why, Some(leave))
    }

    /// The link (or the join) ended under us.
    pub(crate) fn link_lost(&mut self, now: u64, why: FailReason, leave: u16) -> Vec<Action> {
        if self.joined() {
            return self.fail(now, why, Some(leave));
        }
        Vec::new()
    }
}
