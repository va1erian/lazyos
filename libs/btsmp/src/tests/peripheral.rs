//! A small responder (peripheral) for tests: it answers the central the way a
//! keyboard or mouse would, so the real crypto runs on both sides and a wrong
//! `f5` argument order fails the pairing. It shares `func` with the central
//! (the spec vectors pin those) but none of the state-machine code.

use alloc::vec::Vec;

use lazyos_crypto::p256::{KeyPair, PeerPublicKey};
use lazyos_crypto::rng::Entropy;

use crate::func::{f4, f5, f6, g2};
use crate::pairing::DeviceAddr;
use crate::pdu::{keydist, AuthReq, IoCapability, PairingFeatures, Pdu, Reason};

fn rev<const N: usize>(x: &[u8; N]) -> [u8; N] {
    let mut out = *x;
    out.reverse();
    out
}

fn x_be(key: &[u8; 64]) -> [u8; 32] {
    rev(key[..32].try_into().unwrap())
}

pub struct Peripheral {
    pub io: IoCapability,
    /// What the user typed on the keyboard (Passkey Entry only).
    pub typed: Option<u32>,
    pub max_key_size: u8,
    /// Answer without the SC bit, like a legacy-only mouse.
    pub legacy: bool,
    /// Check the central's confirm values; off to let the central catch a
    /// wrong passkey on its own.
    pub verify_ca: bool,
    /// Distribute our identity key.
    pub send_identity: bool,
    pub irk: [u8; 16],
    pub identity: DeviceAddr,
    /// The central's identity, as received.
    pub got_irk: Option<[u8; 16]>,
    pub got_identity: Option<DeviceAddr>,
    /// The LTK in HCI order, masked to the key size.
    pub ltk: Option<[u8; 16]>,
    /// The Numeric Comparison number it would show.
    pub number: Option<u32>,
    local: DeviceAddr,
    central: DeviceAddr,
    keypair: KeyPair,
    pk_a: [u8; 64],
    req: Option<PairingFeatures>,
    rsp: Option<PairingFeatures>,
    na: [u8; 16],
    nb: [u8; 16],
    ca: [u8; 16],
    round: u8,
    mac_key: [u8; 16],
}

impl Peripheral {
    pub fn new(io: IoCapability, local: DeviceAddr, central: DeviceAddr) -> Peripheral {
        let mut rng = Entropy::new();
        rng.seed(b"test peripheral");
        Peripheral {
            io,
            typed: None,
            max_key_size: 16,
            legacy: false,
            verify_ca: true,
            send_identity: true,
            irk: [0x5A; 16],
            identity: local,
            got_irk: None,
            got_identity: None,
            ltk: None,
            number: None,
            local,
            central,
            keypair: KeyPair::generate(&mut rng).unwrap(),
            pk_a: [0; 64],
            req: None,
            rsp: None,
            na: [0; 16],
            nb: [0; 16],
            ca: [0; 16],
            round: 0,
            mac_key: [0; 16],
        }
    }

    /// Use another key pair (the debug key, in the weak-key test).
    pub fn set_keypair(&mut self, keypair: KeyPair) {
        self.keypair = keypair;
    }

    pub fn public_key(&self) -> [u8; 64] {
        self.keypair.public_smp()
    }

    fn passkey_mode(&self) -> bool {
        self.io == IoCapability::KeyboardOnly
    }

    fn z(&self) -> u8 {
        if self.passkey_mode() {
            let bit = (self.typed.unwrap_or(0) >> self.round) & 1;
            0x80 | bit as u8
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

    fn rand16(seed: u8) -> [u8; 16] {
        [seed; 16]
    }

    /// Handle one PDU from the central. `Err` is a failure the peripheral
    /// reports with Pairing Failed.
    pub fn on_pdu(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, Reason> {
        let pdu = Pdu::decode(bytes).map_err(|_| Reason::INVALID_PARAMETERS)?;
        let mut out = Vec::new();
        match pdu {
            Pdu::PairingRequest(req) => {
                let auth = AuthReq::BONDING
                    | AuthReq::MITM
                    | if self.legacy {
                        0
                    } else {
                        AuthReq::SECURE_CONNECTIONS
                    };
                let rsp = PairingFeatures {
                    io_capability: self.io,
                    oob_present: false,
                    auth_req: AuthReq(auth),
                    max_key_size: self.max_key_size,
                    initiator_keys: req.initiator_keys & keydist::ID_KEY,
                    responder_keys: if self.send_identity {
                        req.responder_keys & keydist::ID_KEY
                    } else {
                        0
                    },
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
                self.ca = ca;
                out.push(self.next_confirm());
            }
            Pdu::PairingRandom(na) => {
                if self.passkey_mode() && self.verify_ca {
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
                self.remember_na(na);
                out.push(Pdu::PairingRandom(self.nb).encode());
                self.number = Some(self.numeric(&na));
                self.round += 1;
            }
            Pdu::DhKeyCheck(ea) => out.push(self.dhkey_check(ea)?),
            Pdu::IdentityInfo(irk) => self.got_irk = Some(irk),
            Pdu::IdentityAddressInfo { addr_type, addr } => {
                self.got_identity = Some(DeviceAddr {
                    kind: addr_type,
                    addr,
                })
            }
            _ => return Err(Reason::UNSPECIFIED),
        }
        Ok(out)
    }

    /// The nonce and `Cb` for the current round.
    fn next_confirm(&mut self) -> Vec<u8> {
        self.nb = Self::rand16(0x40 + self.round);
        let cb = f4(
            &x_be(&self.public_key()),
            &x_be(&self.pk_a),
            &rev(&self.nb),
            self.z(),
        );
        Pdu::PairingConfirm(rev(&cb)).encode()
    }

    fn remember_na(&mut self, na: [u8; 16]) {
        self.na = na;
    }

    fn numeric(&self, na: &[u8; 16]) -> u32 {
        g2(
            &x_be(&self.pk_a),
            &x_be(&self.public_key()),
            &rev(na),
            &rev(&self.nb),
        ) % 1_000_000
    }

    fn dhkey_check(&mut self, ea: [u8; 16]) -> Result<Vec<u8>, Reason> {
        let peer = PeerPublicKey::from_smp(&self.pk_a).map_err(|_| Reason::INVALID_PARAMETERS)?;
        let dh = self.keypair.dh_key(&peer);
        let (a, b) = (self.central_spec(), self.local_spec());
        let (na, nb) = (rev(&self.na), rev(&self.nb));
        let (mac, ltk) = f5(&rev(&dh), &na, &nb, &a, &b);
        self.mac_key = mac;
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

    fn central_spec(&self) -> [u8; 7] {
        spec(&self.central)
    }

    fn local_spec(&self) -> [u8; 7] {
        spec(&self.local)
    }

    /// The link is encrypted: send our identity keys if we agreed to.
    pub fn on_encrypted(&mut self) -> Vec<Vec<u8>> {
        let rsp = self.rsp.unwrap();
        if rsp.responder_keys & keydist::ID_KEY == 0 {
            return Vec::new();
        }
        alloc::vec![
            Pdu::IdentityInfo(self.irk).encode(),
            Pdu::IdentityAddressInfo {
                addr_type: self.identity.kind,
                addr: self.identity.addr,
            }
            .encode(),
        ]
    }
}

fn spec(d: &DeviceAddr) -> [u8; 7] {
    let a = d.addr;
    [d.kind as u8, a[5], a[4], a[3], a[2], a[1], a[0]]
}
