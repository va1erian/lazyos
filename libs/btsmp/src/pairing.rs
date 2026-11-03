//! The LE Secure Connections pairing state machine, central (initiator) role.
//!
//! [`Central::step`] is the only entry point: time and one [`Input`] go in, a
//! [`Step`] (PDUs to send, things to show, the outcome, the next deadline)
//! comes out. There is no I/O and no clock here, so every path is a host test.
//!
//! The phases (Core Vol 3, Part H, 2.3.5.6): feature exchange
//! (`on_response`), public keys, authentication stage 1 (confirm/random, one
//! round for Just Works and Numeric Comparison, twenty for Passkey Entry),
//! stage 2 (DHKey check), encryption, key distribution. Stages 1 and 2 are in
//! `auth.rs`, distribution in `dist.rs`.

use alloc::vec::Vec;

use lazyos_crypto::p256::KeyPair;
use lazyos_crypto::rng::Entropy;
use zeroize::Zeroizing;

use crate::func::Addr56;
use crate::method::Method;
use crate::pdu::{keydist, AddrType, AuthReq, IoCapability, PairingFeatures, Pdu, Reason};
use crate::secret::Secret;
use crate::Error;

/// The SMP timeout (Core Vol 3, Part H, 3.4): no progress for 30 s ends the
/// pairing and the link should be dropped.
pub const SMP_TIMEOUT_MS: u64 = 30_000;

/// Rounds of the Passkey Entry bit commitment, one per passkey bit.
pub(crate) const PASSKEY_ROUNDS: u8 = 20;

/// A device address and its type, in HCI order (least significant octet
/// first).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceAddr {
    pub kind: AddrType,
    pub addr: [u8; 6],
}

impl DeviceAddr {
    /// The 56-bit form `f5`/`f6` hash: type then address, most significant
    /// octet first.
    pub(crate) fn spec(&self) -> Addr56 {
        let a = &self.addr;
        [self.kind as u8, a[5], a[4], a[3], a[2], a[1], a[0]]
    }
}

/// The identity we hand the peer so it can recognise us after we change
/// address. A host on a public or static address needs none.
#[derive(Clone, Debug)]
pub struct LocalIdentity {
    pub irk: Secret<16>,
    pub address: DeviceAddr,
}

/// What one pairing needs to know up front.
#[derive(Clone, Debug)]
pub struct Config {
    /// Our address on this connection (initiator `A`).
    pub local: DeviceAddr,
    /// The peer's address on this connection (responder `B`).
    pub peer: DeviceAddr,
    /// Smallest encryption key size accepted; 7..=16, default 16.
    pub min_key_size: u8,
    /// Fail rather than make an unauthenticated (Just Works) key. Set for a
    /// keyboard (plan Security section).
    pub require_authenticated: bool,
    /// Distribute our identity if set.
    pub local_identity: Option<LocalIdentity>,
}

impl Config {
    pub fn new(local: DeviceAddr, peer: DeviceAddr) -> Config {
        Config {
            local,
            peer,
            min_key_size: 16,
            require_authenticated: false,
            local_identity: None,
        }
    }
}

/// An event for [`Central::step`].
#[derive(Clone, Copy, Debug)]
pub enum Input<'a> {
    /// Begin: send the Pairing Request.
    Start,
    /// One SMP SDU received from the peer.
    Pdu(&'a [u8]),
    /// The user's answer to [`Notice::ConfirmNumber`].
    UserConfirm(bool),
    /// The HCI Encryption Change for the link after [`Notice::StartEncryption`].
    /// The transport must deliver it before any later SMP SDU: key PDUs are
    /// only believed once the link is encrypted.
    Encryption { ok: bool },
    /// The user gave up.
    Cancel,
    /// Time passed; checks the deadline.
    Tick,
}

/// Something for the caller to act on.
#[derive(Clone)]
pub enum Notice {
    /// Show these six digits; the peer keyboard types them.
    DisplayPasskey(u32),
    /// Show these six digits and ask whether the peer shows the same.
    ConfirmNumber(u32),
    /// Start link encryption with this key (HCI LE Start Encryption, with
    /// EDIV and Rand zero). `ltk` is in HCI order, already masked to
    /// `key_size`.
    StartEncryption {
        ltk: Secret<16>,
        key_size: u8,
        authenticated: bool,
    },
}

// Passkeys and numbers are as good as keys while a pairing is open.
impl core::fmt::Debug for Notice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Notice::DisplayPasskey(_) => "DisplayPasskey(<redacted>)",
            Notice::ConfirmNumber(_) => "ConfirmNumber(<redacted>)",
            Notice::StartEncryption { .. } => "StartEncryption(<redacted>)",
        })
    }
}

/// The result of a successful pairing, for the bond store.
#[derive(Clone, Debug)]
pub struct Bond {
    /// Long-term key, HCI order, masked to `key_size`.
    pub ltk: Secret<16>,
    pub key_size: u8,
    /// Made with Passkey Entry or Numeric Comparison (MITM protected).
    pub authenticated: bool,
    pub method: Method,
    /// The peer's IRK, if it distributed one.
    pub peer_irk: Option<Secret<16>>,
    /// The peer's identity address, if it distributed one.
    pub peer_identity: Option<DeviceAddr>,
    /// The address the peer used on this connection.
    pub peer_address: DeviceAddr,
}

/// What a step produced.
#[derive(Debug, Default)]
pub struct Step {
    /// SDUs to send on the SMP channel, in order.
    pub send: Vec<Vec<u8>>,
    pub notices: Vec<Notice>,
    /// `Some` once, when the pairing ends.
    pub outcome: Option<Result<Bond, Error>>,
    /// Call `step(now, Input::Tick)` at this time (ms, caller's clock) if
    /// nothing else happened. `None` when the machine is idle or finished.
    pub deadline_ms: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum State {
    Idle,
    WaitResponse,
    WaitPublicKey,
    WaitConfirm,
    WaitRandom,
    WaitUser,
    WaitDhKey,
    KeyDist,
    Done,
    Failed,
}

/// A step that ends the pairing: the error and, when the peer should be told,
/// the Pairing Failed reason.
pub(crate) struct Fail {
    pub error: Error,
    pub reason: Option<Reason>,
}

impl Fail {
    pub fn send(error: Error, reason: Reason) -> Fail {
        Fail {
            error,
            reason: Some(reason),
        }
    }
    pub fn quiet(error: Error) -> Fail {
        Fail {
            error,
            reason: None,
        }
    }
}

/// `Ok(true)` when the step made progress (resets the deadline).
pub(crate) type Handled = Result<bool, Fail>;

/// One LE Secure Connections pairing as the central.
pub struct Central {
    pub(crate) cfg: Config,
    pub(crate) rng: Entropy,
    pub(crate) state: State,
    deadline: u64,
    pub(crate) keypair: Option<KeyPair>,
    pub(crate) req: Option<PairingFeatures>,
    pub(crate) rsp: Option<PairingFeatures>,
    pub(crate) method: Method,
    pub(crate) key_size: u8,
    pub(crate) passkey: u32,
    pub(crate) round: u8,
    pub(crate) pk_a: [u8; 64],
    pub(crate) pk_b: [u8; 64],
    pub(crate) dh: Option<Zeroizing<[u8; 32]>>,
    /// Nonces and the peer's confirm, in air order.
    pub(crate) na: [u8; 16],
    pub(crate) nb: [u8; 16],
    pub(crate) cb: [u8; 16],
    pub(crate) mac_key: Option<Secret<16>>,
    pub(crate) ltk: Option<Secret<16>>,
    pub(crate) dist: crate::dist::Dist,
}

// Hand-written: no field is printed.
impl core::fmt::Debug for Central {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Central({:?})", self.state)
    }
}

impl Central {
    /// A machine for one pairing. `rng` must be seeded by the caller from the
    /// best entropy it has; an unseeded one makes [`Input::Start`] fail with
    /// [`Error::Unseeded`].
    pub fn new(cfg: Config, rng: Entropy) -> Central {
        Central {
            cfg,
            rng,
            state: State::Idle,
            deadline: 0,
            keypair: None,
            req: None,
            rsp: None,
            method: Method::JustWorks,
            key_size: 16,
            passkey: 0,
            round: 0,
            pk_a: [0; 64],
            pk_b: [0; 64],
            dh: None,
            na: [0; 16],
            nb: [0; 16],
            cb: [0; 16],
            mac_key: None,
            ltk: None,
            dist: crate::dist::Dist::default(),
        }
    }

    /// Whether a pairing is in flight (started, not finished).
    pub fn is_active(&self) -> bool {
        !matches!(self.state, State::Idle | State::Done | State::Failed)
    }

    /// Advance the machine. `now_ms` is any monotonic clock in milliseconds.
    pub fn step(&mut self, now_ms: u64, input: Input<'_>) -> Step {
        let mut out = Step::default();
        let startable = self.state == State::Idle && matches!(input, Input::Start);
        if !self.is_active() && !startable {
            return out;
        }
        if self.is_active() && now_ms >= self.deadline {
            self.fail(Fail::quiet(Error::Timeout), &mut out);
            return out;
        }
        match self.handle(input, &mut out) {
            // Only real progress extends the deadline, so a peer cannot hold
            // a pairing open by sending ignorable PDUs.
            Ok(true) => self.deadline = now_ms.saturating_add(SMP_TIMEOUT_MS),
            Ok(false) => {}
            Err(failure) => self.fail(failure, &mut out),
        }
        if self.is_active() {
            out.deadline_ms = Some(self.deadline);
        }
        out
    }

    fn handle(&mut self, input: Input<'_>, out: &mut Step) -> Handled {
        match input {
            Input::Start => self.start(out),
            Input::Cancel => Err(Fail::send(Error::Cancelled, Reason::UNSPECIFIED)),
            Input::Tick => Ok(false),
            Input::UserConfirm(yes) => self.on_user_confirm(yes, out),
            Input::Encryption { ok } => self.on_encryption(ok, out),
            Input::Pdu(bytes) => self.on_pdu(bytes, out),
        }
    }

    fn on_pdu(&mut self, bytes: &[u8], out: &mut Step) -> Handled {
        let pdu = Pdu::decode(bytes).map_err(|e| Fail::send(Error::Malformed(e), e.reason()))?;
        let state = self.state;
        match (state, pdu) {
            (_, Pdu::PairingFailed(reason)) => Err(Fail::quiet(Error::PeerFailed(reason))),
            // Ignorable at any time; neither counts as progress.
            (_, Pdu::SecurityRequest(_) | Pdu::KeypressNotification(_)) => Ok(false),
            (State::WaitResponse, Pdu::PairingResponse(f)) => self.on_response(f, out),
            (State::WaitPublicKey, Pdu::PublicKey(key)) => self.on_public_key(&key, out),
            (State::WaitConfirm, Pdu::PairingConfirm(c)) => self.on_confirm(c, out),
            (State::WaitRandom, Pdu::PairingRandom(n)) => self.on_random(n, out),
            (State::WaitDhKey, Pdu::DhKeyCheck(e)) => self.on_dhkey_check(e, out),
            (State::KeyDist, Pdu::IdentityInfo(irk)) => self.on_identity_info(irk),
            (State::KeyDist, Pdu::IdentityAddressInfo { addr_type, addr }) => {
                self.on_identity_address(addr_type, addr, out)
            }
            (_, other) => {
                let reason = match other {
                    Pdu::PairingRequest(_) => Reason::COMMAND_NOT_SUPPORTED,
                    _ => Reason::UNSPECIFIED,
                };
                Err(Fail::send(
                    Error::UnexpectedPdu { code: other.code() },
                    reason,
                ))
            }
        }
    }

    fn start(&mut self, out: &mut Step) -> Handled {
        let keypair = KeyPair::generate(&mut self.rng).map_err(|_| Fail::quiet(Error::Unseeded))?;
        self.pk_a = keypair.public_smp();
        self.keypair = Some(keypair);
        let request = PairingFeatures {
            io_capability: IoCapability::DisplayYesNo,
            oob_present: false,
            auth_req: AuthReq(AuthReq::BONDING | AuthReq::MITM | AuthReq::SECURE_CONNECTIONS),
            max_key_size: 16,
            initiator_keys: if self.cfg.local_identity.is_some() {
                keydist::ID_KEY
            } else {
                0
            },
            responder_keys: keydist::ID_KEY,
        };
        self.req = Some(request);
        out.send.push(Pdu::PairingRequest(request).encode());
        self.state = State::WaitResponse;
        Ok(true)
    }

    fn on_response(&mut self, rsp: PairingFeatures, out: &mut Step) -> Handled {
        let req = self
            .req
            .ok_or(Fail::quiet(Error::UnexpectedPdu { code: 0 }))?;
        // Legacy pairing's c1/s1 confirm has no public-key step and falls to
        // a brute-forceable TK for Just Works; refuse rather than downgrade.
        if !rsp.auth_req.secure_connections() {
            return Err(Fail::send(
                Error::LegacyPairingRefused,
                Reason::PAIRING_NOT_SUPPORTED,
            ));
        }
        if rsp.oob_present {
            return Err(Fail::send(Error::OobUnsupported, Reason::OOB_NOT_AVAILABLE));
        }
        if rsp.max_key_size < self.cfg.min_key_size {
            return Err(Fail::send(
                Error::KeySizeTooSmall {
                    size: rsp.max_key_size,
                },
                Reason::ENCRYPTION_KEY_SIZE,
            ));
        }
        // A responder may clear distribution bits, never add them.
        if rsp.initiator_keys & !req.initiator_keys != 0
            || rsp.responder_keys & !req.responder_keys != 0
        {
            return Err(Fail::send(
                Error::KeyDistributionMismatch,
                Reason::INVALID_PARAMETERS,
            ));
        }
        let method = Method::select(rsp.io_capability);
        if self.cfg.require_authenticated && !method.authenticated() {
            return Err(Fail::send(
                Error::AuthenticationRequired,
                Reason::AUTHENTICATION_REQUIREMENTS,
            ));
        }
        self.method = method;
        self.key_size = rsp.max_key_size.min(req.max_key_size);
        self.rsp = Some(rsp);
        self.dist = crate::dist::Dist::new(&rsp);
        if method == Method::PasskeyEntry {
            self.passkey = self.random_passkey();
            out.notices.push(Notice::DisplayPasskey(self.passkey));
        }
        out.send.push(Pdu::PublicKey(self.pk_a).encode());
        self.state = State::WaitPublicKey;
        Ok(true)
    }

    /// Six uniform decimal digits. Rejection sampling keeps `% 10^6` unbiased.
    fn random_passkey(&mut self) -> u32 {
        const LIMIT: u32 = 4_294 * 1_000_000;
        loop {
            let mut word = [0u8; 4];
            self.rng.fill(&mut word);
            let value = u32::from_le_bytes(word);
            if value < LIMIT {
                return value % 1_000_000;
            }
        }
    }

    /// End the pairing: tell the peer if there is a reason, drop the secrets,
    /// report the error.
    fn fail(&mut self, failure: Fail, out: &mut Step) {
        if let Some(reason) = failure.reason {
            out.send.push(Pdu::PairingFailed(reason).encode());
        }
        self.state = State::Failed;
        self.wipe();
        out.outcome = Some(Err(failure.error));
        out.deadline_ms = None;
    }

    /// Success: hand the keys over and drop our copies.
    pub(crate) fn complete(&mut self, out: &mut Step) -> Handled {
        let ltk = self
            .ltk
            .take()
            .ok_or(Fail::quiet(Error::EncryptionFailed))?;
        let bond = Bond {
            ltk,
            key_size: self.key_size,
            authenticated: self.method.authenticated(),
            method: self.method,
            peer_irk: self.dist.irk.take(),
            peer_identity: self.dist.identity.take(),
            peer_address: self.cfg.peer,
        };
        self.state = State::Done;
        self.wipe();
        out.outcome = Some(Ok(bond));
        Ok(true)
    }

    fn wipe(&mut self) {
        self.keypair = None;
        self.dh = None;
        self.mac_key = None;
        self.ltk = None;
        self.passkey = 0;
        self.dist.irk = None;
    }
}
