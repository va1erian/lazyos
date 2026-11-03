//! The responder (peripheral) side of LE Secure Connections for a simulated
//! keyboard or mouse. It runs the real crypto (`btsmp::func`, P-256 from
//! `lazyos-crypto`) but none of the host's state machine, so a wrong `f5`
//! argument on either side fails the pairing.
//!
//! A keyboard (KeyboardOnly) waits for "the user" to type the passkey the
//! host displays ([`SmpPeer::type_passkey`]) before it commits to the first
//! round, as a person reading a screen would.

use btsmp::func::{f4, f5, f6};
use btsmp::pdu::{keydist, AuthReq, IoCapability, PairingFeatures, Pdu, Reason};
use btsmp::DeviceAddr;
use lazyos_crypto::p256::{KeyPair, PeerPublicKey};
use lazyos_crypto::rng::Entropy;

fn rev<const N: usize>(x: &[u8; N]) -> [u8; N] {
    let mut out = *x;
    out.reverse();
    out
}

fn x_be(key: &[u8; 64]) -> [u8; 32] {
    let mut x = [0u8; 32];
    x.copy_from_slice(&key[..32]);
    rev(&x)
}

fn spec(d: &DeviceAddr) -> [u8; 7] {
    let a = d.addr;
    [d.kind as u8, a[5], a[4], a[3], a[2], a[1], a[0]]
}

pub struct SmpPeer {
    pub io: IoCapability,
    /// The passkey "typed" on the keyboard (Passkey Entry only).
    typed: Option<u32>,
    irk: [u8; 16],
    identity: DeviceAddr,
    local: DeviceAddr,
    central: DeviceAddr,
    keypair: KeyPair,
    pk_a: [u8; 64],
    req: Option<PairingFeatures>,
    rsp: Option<PairingFeatures>,
    na: [u8; 16],
    nb: [u8; 16],
    ca: [u8; 16],
    /// A Pairing Confirm that arrived before the passkey was typed.
    held_confirm: Option<[u8; 16]>,
    round: u8,
    /// The long term key, HCI order, masked to the key size.
    pub ltk: Option<[u8; 16]>,
}

impl SmpPeer {
    /// `local` is the address the peripheral uses on this connection,
    /// `central` the host's; `identity` and `irk` are what it distributes.
    pub fn new(
        io: IoCapability,
        local: DeviceAddr,
        central: DeviceAddr,
        identity: DeviceAddr,
        irk: [u8; 16],
        seed: &[u8],
    ) -> Option<SmpPeer> {
        let mut rng = Entropy::new();
        rng.seed(seed);
        Some(SmpPeer {
            io,
            typed: None,
            irk,
            identity,
            local,
            central,
            keypair: KeyPair::generate(&mut rng).ok()?,
            pk_a: [0; 64],
            req: None,
            rsp: None,
            na: [0; 16],
            nb: [0; 16],
            ca: [0; 16],
            held_confirm: None,
            round: 0,
            ltk: None,
        })
    }

    fn public_key(&self) -> [u8; 64] {
        self.keypair.public_smp()
    }

    fn passkey_mode(&self) -> bool {
        self.io == IoCapability::KeyboardOnly
    }

    /// Whether the pairing is waiting for the user to type.
    pub fn wants_passkey(&self) -> bool {
        self.passkey_mode() && self.typed.is_none() && self.req.is_some()
    }

    /// The user typed `passkey`; continue with the confirm that was held.
    pub fn type_passkey(&mut self, passkey: u32) -> Result<Vec<Vec<u8>>, Reason> {
        self.typed = Some(passkey);
        match self.held_confirm.take() {
            Some(ca) => self.on_pdu(&Pdu::PairingConfirm(ca).encode()),
            None => Ok(Vec::new()),
        }
    }

    fn z(&self) -> u8 {
        if self.passkey_mode() {
            0x80 | ((self.typed.unwrap_or(0) >> self.round) & 1) as u8
        } else {
            0
        }
    }

    fn r_value(&self) -> [u8; 16] {
        let mut r = [0u8; 16];
        if self.passkey_mode() {
            r[12..].copy_from_slice(&self.typed.unwrap_or(0).to_be_bytes());
        }
        r
    }

    /// One PDU from the central; `Err` is the reason for Pairing Failed.
    pub fn on_pdu(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, Reason> {
        let pdu = Pdu::decode(bytes).map_err(|_| Reason::INVALID_PARAMETERS)?;
        let mut out = Vec::new();
        match pdu {
            Pdu::PairingRequest(req) => {
                let rsp = PairingFeatures {
                    io_capability: self.io,
                    oob_present: false,
                    auth_req: AuthReq(
                        AuthReq::BONDING | AuthReq::MITM | AuthReq::SECURE_CONNECTIONS,
                    ),
                    max_key_size: 16,
                    initiator_keys: req.initiator_keys & keydist::ID_KEY,
                    responder_keys: req.responder_keys & keydist::ID_KEY,
                };
                self.req = Some(req);
                self.rsp = Some(rsp);
                out.push(Pdu::PairingResponse(rsp).encode());
            }
            Pdu::PublicKey(key) => {
                self.pk_a = key;
                out.push(Pdu::PublicKey(self.public_key()).encode());
                if !self.passkey_mode() {
                    out.push(self.next_confirm());
                }
            }
            Pdu::PairingConfirm(ca) => {
                if self.passkey_mode() && self.typed.is_none() {
                    self.held_confirm = Some(ca);
                    return Ok(out);
                }
                self.ca = ca;
                out.push(self.next_confirm());
            }
            Pdu::PairingRandom(na) => {
                if self.passkey_mode() {
                    let want = rev(&f4(
                        &x_be(&self.pk_a),
                        &x_be(&self.public_key()),
                        &rev(&na),
                        self.z(),
                    ));
                    if want != self.ca {
                        return Err(Reason::CONFIRM_VALUE_FAILED);
                    }
                }
                self.na = na;
                out.push(Pdu::PairingRandom(self.nb).encode());
                self.round += 1;
            }
            Pdu::DhKeyCheck(ea) => out.push(self.dhkey_check(ea)?),
            // The central's identity, if it ever sends one.
            Pdu::IdentityInfo(_) | Pdu::IdentityAddressInfo { .. } => {}
            _ => return Err(Reason::UNSPECIFIED),
        }
        Ok(out)
    }

    /// The nonce and `Cb` for the current round.
    fn next_confirm(&mut self) -> Vec<u8> {
        self.nb = [0x40 + self.round; 16];
        let cb = f4(
            &x_be(&self.public_key()),
            &x_be(&self.pk_a),
            &rev(&self.nb),
            self.z(),
        );
        Pdu::PairingConfirm(rev(&cb)).encode()
    }

    fn dhkey_check(&mut self, ea: [u8; 16]) -> Result<Vec<u8>, Reason> {
        let peer = PeerPublicKey::from_smp(&self.pk_a).map_err(|_| Reason::INVALID_PARAMETERS)?;
        let dh = self.keypair.dh_key(&peer);
        let (a, b) = (spec(&self.central), spec(&self.local));
        let (na, nb) = (rev(&self.na), rev(&self.nb));
        let (mac, ltk) = f5(&rev(&dh), &na, &nb, &a, &b);
        let req = self.req.ok_or(Reason::UNSPECIFIED)?;
        let rsp = self.rsp.ok_or(Reason::UNSPECIFIED)?;
        let want = f6(&mac, &na, &nb, &self.r_value(), &req.io_cap_bytes(), &a, &b);
        if rev(&want) != ea {
            return Err(Reason::DHKEY_CHECK_FAILED);
        }
        let mut ltk = rev(&ltk);
        let size = usize::from(req.max_key_size.min(rsp.max_key_size));
        ltk[size..].fill(0);
        self.ltk = Some(ltk);
        let eb = f6(&mac, &nb, &na, &self.r_value(), &rsp.io_cap_bytes(), &b, &a);
        Ok(Pdu::DhKeyCheck(rev(&eb)).encode())
    }

    /// Whether the pairing made an authenticated key (the user typed).
    pub fn authenticated(&self) -> bool {
        self.passkey_mode()
    }

    /// The link is encrypted: send the identity keys the central asked for.
    pub fn on_encrypted(&mut self) -> Vec<Vec<u8>> {
        let Some(rsp) = self.rsp else {
            return Vec::new();
        };
        if rsp.responder_keys & keydist::ID_KEY == 0 {
            return Vec::new();
        }
        vec![
            Pdu::IdentityInfo(self.irk).encode(),
            Pdu::IdentityAddressInfo {
                addr_type: self.identity.kind,
                addr: self.identity.addr,
            }
            .encode(),
        ]
    }
}
