//! The hooks the pairing code calls so a caller of `os.lazy.bt.v1` sees the
//! pairing move: passkey to show, number to compare, paired, failed.
//!
//! Only the device the window was opened for is reported on; a pairing that
//! no `Pair` call asked for (the harness's `pair=` argument) is silent here,
//! and a number to compare is declined unless a caller can answer it.

use bthci::BdAddr;

use crate::ctl_types::*;
use crate::host::Host;

impl Host {
    /// Whether a `Pair` call is waiting on `addr`.
    fn ctl_waits_on(&self, addr: BdAddr) -> bool {
        let state = self.ctl.pairing;
        state.phase.active() && state.addr == Some(addr)
    }

    /// The passkey the keyboard's owner must type.
    pub(crate) fn ctl_passkey(&mut self, addr: BdAddr, passkey: u32) {
        if self.ctl_waits_on(addr) {
            self.set_pairing(PairingState {
                phase: Phase::ShowPasskey,
                passkey,
                ..self.ctl.pairing
            });
        }
    }

    /// A number both screens must show. Returns whether a caller can answer
    /// it; if not, the pairing code declines it.
    pub(crate) fn ctl_number(&mut self, addr: BdAddr, number: u32) -> bool {
        if !self.ctl_waits_on(addr) {
            return false;
        }
        self.set_pairing(PairingState {
            phase: Phase::ConfirmNumber,
            number,
            ..self.ctl.pairing
        });
        true
    }

    /// The device is bonded and acceptable.
    pub(crate) fn ctl_paired(&mut self, now: u64, addr: BdAddr) {
        if self.ctl_waits_on(addr) {
            self.set_pairing(PairingState {
                phase: Phase::Paired,
                passkey: 0,
                number: 0,
                ..self.ctl.pairing
            });
            self.end_pairing_window(now);
        }
        self.ctl_changed(addr);
    }

    /// The pairing ended without a bond. Repeats are ignored: the first
    /// reason is the truth.
    pub(crate) fn ctl_failed(&mut self, addr: BdAddr, reason: FailReason) {
        if !self.ctl_waits_on(addr) {
            return;
        }
        self.set_pairing(PairingState {
            phase: Phase::Failed,
            passkey: 0,
            number: 0,
            reason,
            ..self.ctl.pairing
        });
        let now = self.last_cmd_ms;
        self.end_pairing_window(now);
    }

    /// A link went down: a pairing that had not finished on it is lost.
    pub(crate) fn ctl_link_lost(&mut self, addr: BdAddr) {
        // A link that drops after the bond was made is not a failure; one
        // that drops before is (the phase is still active).
        self.ctl_failed(addr, FailReason::LinkLost);
    }
}

/// How SMP ended, as the caller sees it.
pub(crate) fn smp_fail_reason(error: &btsmp::Error) -> FailReason {
    use btsmp::Error::*;
    match error {
        Timeout => FailReason::Timeout,
        Cancelled => FailReason::Cancelled,
        UserRejected | ConfirmMismatch => FailReason::Rejected,
        LegacyPairingRefused | KeySizeTooSmall { .. } | OobUnsupported | Unseeded => {
            FailReason::Unsupported
        }
        _ => FailReason::AuthFailed,
    }
}

/// How a BR/EDR Simple Pairing status reads.
pub(crate) fn ssp_fail_reason(code: u8) -> FailReason {
    use bthci::opcode::status;
    match code {
        status::PAIRING_NOT_ALLOWED | status::REMOTE_USER_TERMINATED => FailReason::Rejected,
        _ => FailReason::AuthFailed,
    }
}
