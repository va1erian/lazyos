//! Authentication stages 1 and 2 of LE Secure Connections (Core Vol 3,
//! Part H, 2.3.5.6.2 to 2.3.5.6.5) for the initiator.
//!
//! Values are held in air order (little-endian); each call into `func` reverses
//! its inputs to the specification's form and reverses the result back, so
//! the arithmetic reads like the specification.

use lazyos_crypto::p256::PeerPublicKey;
use lazyos_crypto::wifi::mic_eq;

use crate::func::{f4, f5, f6, g2};
use crate::method::Method;
use crate::pairing::{Central, Fail, Handled, Notice, State, Step, PASSKEY_ROUNDS};
use crate::pdu::{Pdu, Reason};
use crate::secret::Secret;
use crate::Error;

/// X coordinate of the Core Specification's debug public key (Vol 3, Part H,
/// 2.3.5.6.1), most significant octet first. Its private key is published, so
/// a peer using it hands every sniffer the DH key.
const DEBUG_KEY_X: [u8; 32] = [
    0x20, 0xb0, 0x03, 0xd2, 0xf2, 0x97, 0xbe, 0x2c, 0x5e, 0x2c, 0x83, 0xa7, 0xe9, 0xf9, 0xa5, 0xb9,
    0xef, 0xf4, 0x91, 0x11, 0xac, 0xf4, 0xfd, 0xdb, 0xcc, 0x03, 0x01, 0x48, 0x0e, 0x35, 0x9d, 0xe6,
];

fn rev<const N: usize>(x: &[u8; N]) -> [u8; N] {
    let mut out = *x;
    out.reverse();
    out
}

/// The X coordinate of an SMP public key, in specification form.
fn x_be(key: &[u8; 64]) -> [u8; 32] {
    let mut x = [0u8; 32];
    x.copy_from_slice(&key[..32]);
    rev(&x)
}

impl Central {
    /// Z of `f4` for the current round: Passkey Entry commits to one passkey
    /// bit per round (`0x80 | bit`), the other models use zero.
    fn round_z(&self) -> u8 {
        match self.method {
            Method::PasskeyEntry => 0x80 | ((self.passkey >> self.round) & 1) as u8,
            _ => 0,
        }
    }

    /// The 128-bit `r` of `f6`: the passkey, or zero.
    fn r_value(&self) -> [u8; 16] {
        let mut r = [0u8; 16];
        if self.method == Method::PasskeyEntry {
            r[12..].copy_from_slice(&self.passkey.to_be_bytes());
        }
        r
    }

    /// A confirm value in air order: `f4(U, V, nonce, z)` over the keys' X.
    fn confirm(&self, u: &[u8; 64], v: &[u8; 64], nonce: &[u8; 16]) -> [u8; 16] {
        rev(&f4(&x_be(u), &x_be(v), &rev(nonce), self.round_z()))
    }

    pub(crate) fn on_public_key(&mut self, key: &[u8; 64], out: &mut Step) -> Handled {
        let invalid = |error| Fail::send(error, Reason::INVALID_PARAMETERS);
        let peer = PeerPublicKey::from_smp(key).map_err(|_| invalid(Error::InvalidPublicKey))?;
        if *key == self.pk_a || x_be(key) == DEBUG_KEY_X {
            return Err(invalid(Error::WeakPublicKey));
        }
        let keypair = self.keypair.as_ref().ok_or(Fail::quiet(Error::Unseeded))?;
        self.dh = Some(keypair.dh_key(&peer));
        self.pk_b = *key;
        self.start_round(out);
        self.state = State::WaitConfirm;
        Ok(true)
    }

    /// Draw this round's nonce; Passkey Entry commits to it first.
    fn start_round(&mut self, out: &mut Step) {
        self.rng.fill(&mut self.na);
        if self.method == Method::PasskeyEntry {
            let ca = self.confirm(&self.pk_a, &self.pk_b, &self.na);
            out.send.push(Pdu::PairingConfirm(ca).encode());
        }
    }

    pub(crate) fn on_confirm(&mut self, confirm: [u8; 16], out: &mut Step) -> Handled {
        self.cb = confirm;
        out.send.push(Pdu::PairingRandom(self.na).encode());
        self.state = State::WaitRandom;
        Ok(true)
    }

    pub(crate) fn on_random(&mut self, nb: [u8; 16], out: &mut Step) -> Handled {
        self.nb = nb;
        let expected = self.confirm(&self.pk_b, &self.pk_a, &nb);
        if !mic_eq(&expected, &self.cb) {
            return Err(Fail::send(
                Error::ConfirmMismatch,
                Reason::CONFIRM_VALUE_FAILED,
            ));
        }
        if self.method == Method::PasskeyEntry && self.round + 1 < PASSKEY_ROUNDS {
            self.round += 1;
            self.start_round(out);
            self.state = State::WaitConfirm;
            return Ok(true);
        }
        if self.method == Method::NumericComparison {
            let value = g2(
                &x_be(&self.pk_a),
                &x_be(&self.pk_b),
                &rev(&self.na),
                &rev(&nb),
            );
            out.notices.push(Notice::ConfirmNumber(value % 1_000_000));
            self.state = State::WaitUser;
            return Ok(true);
        }
        self.send_dhkey_check(out)
    }

    pub(crate) fn on_user_confirm(&mut self, yes: bool, out: &mut Step) -> Handled {
        if self.state != State::WaitUser {
            return Ok(false);
        }
        if !yes {
            return Err(Fail::send(
                Error::UserRejected,
                Reason::NUMERIC_COMPARISON_FAILED,
            ));
        }
        self.send_dhkey_check(out)
    }

    /// Stage 2: derive MacKey and LTK, send `Ea`.
    fn send_dhkey_check(&mut self, out: &mut Step) -> Handled {
        let dh = self.dh.as_ref().ok_or(Fail::quiet(Error::Unseeded))?;
        let (a, b) = (self.cfg.local.spec(), self.cfg.peer.spec());
        let (mac_key, ltk) = f5(&rev(dh), &rev(&self.na), &rev(&self.nb), &a, &b);
        let ea = f6(
            &mac_key,
            &rev(&self.na),
            &rev(&self.nb),
            &self.r_value(),
            &self.req_io(),
            &a,
            &b,
        );
        // MacKey stays in specification form (only `f6` reads it). The LTK
        // goes to the controller in HCI order with the octets above the
        // negotiated key size zeroed (2.4.4).
        let mut ltk = rev(&ltk);
        ltk[usize::from(self.key_size)..].fill(0);
        self.mac_key = Some(Secret::new(mac_key));
        self.ltk = Some(Secret::new(ltk));
        out.send.push(Pdu::DhKeyCheck(rev(&ea)).encode());
        self.state = State::WaitDhKey;
        Ok(true)
    }

    pub(crate) fn on_dhkey_check(&mut self, eb: [u8; 16], out: &mut Step) -> Handled {
        let mac_key = self.mac_key.as_ref().ok_or(Fail::quiet(Error::Unseeded))?;
        let (a, b) = (self.cfg.local.spec(), self.cfg.peer.spec());
        let expected = rev(&f6(
            mac_key.expose(),
            &rev(&self.nb),
            &rev(&self.na),
            &self.r_value(),
            &self.rsp_io(),
            &b,
            &a,
        ));
        if !mic_eq(&expected, &eb) {
            return Err(Fail::send(
                Error::DhKeyCheckMismatch,
                Reason::DHKEY_CHECK_FAILED,
            ));
        }
        let ltk = self.ltk.clone().ok_or(Fail::quiet(Error::Unseeded))?;
        out.notices.push(Notice::StartEncryption {
            ltk,
            key_size: self.key_size,
            authenticated: self.method.authenticated(),
        });
        self.state = State::KeyDist;
        Ok(true)
    }

    fn req_io(&self) -> [u8; 3] {
        self.req.map(|f| f.io_cap_bytes()).unwrap_or_default()
    }

    fn rsp_io(&self) -> [u8; 3] {
        self.rsp.map(|f| f.io_cap_bytes()).unwrap_or_default()
    }
}
