//! Phase 3 of pairing: once the link is encrypted the peer sends its keys,
//! then we send ours (Core Vol 3, Part H, 2.4.3). In LE Secure Connections the
//! LTK is derived, never sent, so only the identity keys travel.

use crate::pairing::{Central, DeviceAddr, Fail, Handled, State, Step};
use crate::pdu::{keydist, AddrType, PairingFeatures, Pdu, Reason};
use crate::secret::Secret;
use crate::Error;

/// Progress of the key distribution.
#[derive(Default)]
pub(crate) struct Dist {
    /// The Encryption Change event arrived: key PDUs are believed from now.
    encrypted: bool,
    /// The peer agreed to send its IRK and identity address.
    peer_id_expected: bool,
    /// The peer asked for ours.
    send_ours: bool,
    pub irk: Option<Secret<16>>,
    pub identity: Option<DeviceAddr>,
}

impl Dist {
    pub fn new(rsp: &PairingFeatures) -> Dist {
        Dist {
            peer_id_expected: rsp.responder_keys & keydist::ID_KEY != 0,
            send_ours: rsp.initiator_keys & keydist::ID_KEY != 0,
            ..Dist::default()
        }
    }
}

impl Central {
    /// The HCI Encryption Change for the link we started encrypting.
    pub(crate) fn on_encryption(&mut self, ok: bool, out: &mut Step) -> Handled {
        if self.state != State::KeyDist || self.dist.encrypted {
            // Not ours to act on: an event for an older encryption.
            return Ok(false);
        }
        if !ok {
            return Err(Fail::quiet(Error::EncryptionFailed));
        }
        self.dist.encrypted = true;
        self.progress_dist(out)
    }

    pub(crate) fn on_identity_info(&mut self, irk: [u8; 16]) -> Handled {
        // Before encryption a stranger on the air could have written this.
        if !self.dist.encrypted || !self.dist.peer_id_expected || self.dist.irk.is_some() {
            return Err(self.unexpected(Pdu::IdentityInfo(irk).code()));
        }
        self.dist.irk = Some(Secret::new(irk));
        Ok(true)
    }

    pub(crate) fn on_identity_address(
        &mut self,
        kind: AddrType,
        addr: [u8; 6],
        out: &mut Step,
    ) -> Handled {
        let code = crate::pdu::code::IDENTITY_ADDRESS_INFO;
        if !self.dist.encrypted
            || !self.dist.peer_id_expected
            || self.dist.irk.is_none()
            || self.dist.identity.is_some()
        {
            return Err(self.unexpected(code));
        }
        // An identity address is public or random *static* (top bits 11); a
        // private address cannot identify a device across connections.
        if kind == AddrType::Random && addr[5] & 0xC0 != 0xC0 {
            return Err(Fail::send(
                Error::InvalidIdentity,
                Reason::INVALID_PARAMETERS,
            ));
        }
        self.dist.identity = Some(DeviceAddr { kind, addr });
        self.progress_dist(out)
    }

    /// Wait for the rest of the peer's keys, then send ours and finish.
    fn progress_dist(&mut self, out: &mut Step) -> Handled {
        let waiting =
            self.dist.peer_id_expected && (self.dist.irk.is_none() || self.dist.identity.is_none());
        if waiting {
            return Ok(true);
        }
        if let (true, Some(identity)) = (self.dist.send_ours, self.cfg.local_identity.as_ref()) {
            out.send
                .push(Pdu::IdentityInfo(*identity.irk.expose()).encode());
            out.send.push(
                Pdu::IdentityAddressInfo {
                    addr_type: identity.address.kind,
                    addr: identity.address.addr,
                }
                .encode(),
            );
        }
        self.complete(out)
    }

    fn unexpected(&self, code: u8) -> Fail {
        Fail::send(Error::UnexpectedPdu { code }, Reason::UNSPECIFIED)
    }
}
