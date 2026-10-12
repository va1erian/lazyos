//! The join: Open System authentication, association, the WPA2 4-way
//! handshake through `eapol`, and what ends a link.

use alloc::vec;
use alloc::vec::Vec;

use eapol::{Cipher, Config, Key, Stage, Standard, Supplicant};

use crate::failure::BAD_PASSPHRASE_REASONS;
use crate::frames::{self, Rx};
use crate::machine::{Joining, Link, Phase, Station};
use crate::select::{Candidate, Join};
use crate::types::*;
use crate::{reason, HANDSHAKE_TIMEOUT_MS, JOIN_TIMEOUT_MS, JOIN_TRIES};

impl<N: FnMut() -> [u8; 32] + Clone> Station<N> {
    /// Tune to the BSS and send the first authentication request.
    pub(crate) fn start_auth(
        &mut self,
        now: u64,
        cand: Candidate,
        pmk: Option<Key<32>>,
    ) -> Vec<Action> {
        let seq = self.next_seq();
        let join = Action::Join {
            bssid: cand.bss.bssid,
            channel: cand.bss.channel,
            band: cand.band,
        };
        let auth = Action::Tx {
            kind: FrameKind::Mgmt,
            frame: frames::auth_request(&self.mac, &cand.bss.bssid, seq),
        };
        self.phase = Phase::Auth(Joining {
            cand,
            pmk,
            tries: 1,
            deadline: now + JOIN_TIMEOUT_MS,
            rsn_ie: None,
        });
        self.report(ConnState::Authenticating, FailReason::None);
        vec![join, auth]
    }

    /// Authenticated: say so to the radio and ask to associate.
    fn start_assoc(&mut self, now: u64, mut joining: Joining) -> Vec<Action> {
        let seq = self.next_seq();
        let c = &joining.cand;
        let (frame, rsn_ie) = frames::assoc_request(&self.mac, &c.bss, c.band, c.join, seq);
        joining.rsn_ie = rsn_ie;
        joining.tries = 1;
        joining.deadline = now + JOIN_TIMEOUT_MS;
        self.phase = Phase::Assoc(joining);
        self.report(ConnState::Associating, FailReason::None);
        vec![
            Action::SetState {
                state: StaState::Authenticated,
                aid: 0,
            },
            Action::Tx {
                kind: FrameKind::Mgmt,
                frame,
            },
        ]
    }

    /// Associated: open networks are done; WPA2 starts the handshake.
    fn associated(&mut self, now: u64, joining: Joining, aid: u16) -> Vec<Action> {
        let mut out = vec![Action::SetState {
            state: StaState::Associated,
            aid,
        }];
        let Joining {
            cand, pmk, rsn_ie, ..
        } = joining;
        let Join::Psk(akm) = cand.join else {
            out.push(Action::SetState {
                state: StaState::Authorized,
                aid,
            });
            self.phase = Phase::Connected(Link {
                cand,
                aid,
                sup: None,
                deadline: 0,
                msg2_sent: false,
            });
            self.connected(now);
            return out;
        };
        let config = Config {
            akm,
            pairwise: Cipher::Ccmp128,
            group: Cipher::Ccmp128,
            pmk: pmk.unwrap_or_else(|| Key::new([0; 32])),
            aa: cand.bss.bssid,
            spa: self.mac,
            ap_rsn_ie: cand.bss.rsn_ie.clone().unwrap_or_default(),
            assoc_rsn_ie: rsn_ie.unwrap_or_default(),
        };
        match Supplicant::new(config, Standard::new(self.nonce.clone())) {
            Ok(sup) => {
                self.phase = Phase::Handshake(Link {
                    cand,
                    aid,
                    sup: Some(sup),
                    deadline: now + HANDSHAKE_TIMEOUT_MS,
                    msg2_sent: false,
                });
                self.report(ConnState::Handshake, FailReason::None);
                out
            }
            Err(error) => {
                // Selection only picks what eapol accepts; a refusal here is
                // the AP's element changing under us.
                self.phase = Phase::Handshake(Link {
                    cand,
                    aid,
                    sup: None,
                    deadline: now,
                    msg2_sent: false,
                });
                out.extend(self.fail(now, refusal(error), error.deauth_reason()));
                out
            }
        }
    }

    /// A frame from the radio. Only frames from the joined BSS count; the
    /// rest are dropped without any change.
    pub(crate) fn rx(&mut self, now: u64, kind: FrameKind, rssi: i32, frame: &[u8]) -> Vec<Action> {
        let Some(bssid) = self.current().map(|c| c.bss.bssid) else {
            self.dropped += 1;
            return Vec::new();
        };
        let actions = match kind {
            FrameKind::Mgmt => match frames::classify(frame, &self.mac, &bssid) {
                Rx::Ignored => None,
                rx => self.mgmt(now, rx),
            },
            FrameKind::Eapol => match frames::eapol_pdu(frame, &self.mac, &bssid) {
                Some(pdu) => self.eapol(now, pdu),
                None => None,
            },
        };
        match actions {
            Some(actions) => {
                if self.current().is_some_and(|c| c.bss.bssid == bssid) {
                    self.status.rssi_dbm = rssi;
                }
                actions
            }
            None => {
                self.dropped += 1;
                Vec::new()
            }
        }
    }

    /// A management frame from the BSS: `None` when out of place.
    fn mgmt(&mut self, now: u64, rx: Rx) -> Option<Vec<Action>> {
        match (rx, &self.phase) {
            (Rx::Ended { reason }, _) => Some(self.ended(now, reason)),
            (Rx::AuthResponse { status }, Phase::Auth(_)) => {
                let Phase::Auth(joining) = core::mem::replace(&mut self.phase, Phase::Idle) else {
                    return None;
                };
                if frames::success(status) {
                    return Some(self.start_assoc(now, joining));
                }
                self.phase = Phase::Auth(joining);
                Some(self.fail(now, FailReason::Rejected, Some(reason::UNSPECIFIED)))
            }
            (Rx::AssocResponse { status, aid }, Phase::Assoc(_)) => {
                let Phase::Assoc(joining) = core::mem::replace(&mut self.phase, Phase::Idle) else {
                    return None;
                };
                if frames::success(status) {
                    return Some(self.associated(now, joining, aid));
                }
                self.phase = Phase::Assoc(joining);
                Some(self.fail(now, FailReason::Rejected, Some(reason::UNSPECIFIED)))
            }
            _ => None,
        }
    }

    /// An EAPOL PDU from the BSS, during or after the handshake.
    fn eapol(&mut self, now: u64, pdu: &[u8]) -> Option<Vec<Action>> {
        let (Phase::Handshake(link) | Phase::Connected(link)) = &mut self.phase else {
            return None;
        };
        let sup = link.sup.as_mut()?;
        let result = sup.input(pdu);
        if result.is_ok() && sup.stage() == Stage::WaitMsg3 {
            link.msg2_sent = true;
        }
        let (bssid, aid) = (link.cand.bss.bssid, link.aid);
        let steps = match result {
            Ok(steps) => steps,
            Err(error) if error.is_fatal() => {
                return Some(self.fail(now, refusal(error), error.deauth_reason()));
            }
            // A wrong MIC, a replay, a message out of order: drop it and let
            // the handshake deadline (or the AP) decide.
            Err(_) => return None,
        };
        let mut out = Vec::new();
        let mut authorized = false;
        for step in steps {
            out.push(match step {
                eapol::Action::Send(pdu) => Action::Tx {
                    kind: FrameKind::Eapol,
                    frame: frames::eapol_frame(&bssid, &self.mac, &pdu),
                },
                eapol::Action::InstallPtk { tk, .. } => Action::SetKey {
                    kind: KeyKind::Pairwise,
                    index: 0,
                    key: tk,
                    rsc: [0; 8],
                    addr: bssid,
                },
                eapol::Action::InstallGtk { index, key, rsc, .. } => Action::SetKey {
                    kind: KeyKind::Group,
                    index,
                    key,
                    rsc,
                    addr: bssid,
                },
                eapol::Action::Authorized => {
                    authorized = true;
                    Action::SetState {
                        state: StaState::Authorized,
                        aid,
                    }
                }
            });
        }
        if authorized && matches!(self.phase, Phase::Handshake(_)) {
            if let Phase::Handshake(link) = core::mem::replace(&mut self.phase, Phase::Idle) {
                self.phase = Phase::Connected(link);
                self.connected(now);
            }
        }
        Some(out)
    }

    /// The AP deauthenticated or disassociated us (event or frame) with
    /// reason `code`. Unprotected, so anyone can send one: during a
    /// handshake only the reasons an AP gives for a failed message 2 count
    /// towards a wrong passphrase, and even those decay
    /// ([`crate::memory::BLOCK_MS`]).
    pub(crate) fn ended(&mut self, now: u64, code: u16) -> Vec<Action> {
        match &self.phase {
            Phase::Handshake(link) if link.msg2_sent => {
                let strong = BAD_PASSPHRASE_REASONS.contains(&code);
                self.handshake_failed(now, strong, FailReason::Rejected, reason::UNSPECIFIED)
            }
            Phase::Auth(_) | Phase::Assoc(_) | Phase::Handshake(_) => {
                self.link_lost(now, FailReason::Rejected, reason::UNSPECIFIED)
            }
            _ => self.link_lost(now, FailReason::Deauthenticated, reason::UNSPECIFIED),
        }
    }

    /// Retransmissions and the handshake deadline.
    pub(crate) fn join_tick(&mut self, now: u64) -> Vec<Action> {
        let due = match &self.phase {
            Phase::WaitPmk { deadline, .. } => now >= *deadline,
            Phase::Auth(j) | Phase::Assoc(j) => now >= j.deadline,
            Phase::Handshake(link) => now >= link.deadline,
            _ => false,
        };
        if !due {
            return Vec::new();
        }
        match core::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Auth(mut j) if j.tries < JOIN_TRIES => {
                j.tries += 1;
                j.deadline = now + JOIN_TIMEOUT_MS;
                let seq = self.next_seq();
                let frame = frames::auth_request(&self.mac, &j.cand.bss.bssid, seq);
                self.phase = Phase::Auth(j);
                vec![Action::Tx {
                    kind: FrameKind::Mgmt,
                    frame,
                }]
            }
            Phase::Assoc(mut j) if j.tries < JOIN_TRIES => {
                j.tries += 1;
                j.deadline = now + JOIN_TIMEOUT_MS;
                let seq = self.next_seq();
                let c = &j.cand;
                let (frame, _) = frames::assoc_request(&self.mac, &c.bss, c.band, c.join, seq);
                self.phase = Phase::Assoc(j);
                vec![Action::Tx {
                    kind: FrameKind::Mgmt,
                    frame,
                }]
            }
            Phase::Handshake(link) => {
                let msg2_sent = link.msg2_sent;
                self.phase = Phase::Handshake(link);
                if msg2_sent {
                    return self.handshake_failed(now, false, FailReason::Timeout, reason::HANDSHAKE_TIMEOUT);
                }
                self.fail(now, FailReason::Timeout, Some(reason::HANDSHAKE_TIMEOUT))
            }
            phase => {
                self.phase = phase;
                self.fail(now, FailReason::Timeout, Some(reason::UNSPECIFIED))
            }
        }
    }
}

/// What a fatal `eapol` error says about the network: a cipher, AKM or
/// protection the station cannot run is `Unsupported`; a group key it could
/// not take is the AP's fault (`Rejected`); anything else (an RSN element
/// that changed between the beacon and message 3: a downgrade) is
/// `PolicyMismatch`.
fn refusal(error: eapol::Error) -> FailReason {
    match error {
        eapol::Error::UnsupportedAkm(_)
        | eapol::Error::UnsupportedCipher(_)
        | eapol::Error::PmfRequired
        | eapol::Error::NotOffered => FailReason::Unsupported,
        eapol::Error::BadKeyData | eapol::Error::MissingGtk | eapol::Error::BadGtk => {
            FailReason::Rejected
        }
        _ => FailReason::PolicyMismatch,
    }
}
