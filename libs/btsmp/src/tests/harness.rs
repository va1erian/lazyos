//! Runs a [`Central`] against a [`Peripheral`] in memory, with hooks to
//! tamper with PDUs in flight and to play the user.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use lazyos_crypto::rng::Entropy;

use super::peripheral::Peripheral;
use crate::pairing::{Bond, Central, Config, DeviceAddr, Input, LocalIdentity, Notice};
use crate::pdu::{AddrType, IoCapability, Pdu};
use crate::secret::Secret;
use crate::Error;

pub const CENTRAL_ADDR: DeviceAddr = DeviceAddr {
    kind: AddrType::Public,
    addr: [0xCE, 0xBF, 0x37, 0x37, 0x12, 0x56],
};
pub const PERIPHERAL_ADDR: DeviceAddr = DeviceAddr {
    kind: AddrType::Random,
    addr: [0xC1, 0xCF, 0x2D, 0x70, 0x13, 0xE7],
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    ToPeripheral,
    ToCentral,
}

pub fn seeded(label: &[u8]) -> Entropy {
    let mut rng = Entropy::new();
    rng.seed(label);
    rng
}

pub fn config() -> Config {
    let mut cfg = Config::new(CENTRAL_ADDR, PERIPHERAL_ADDR);
    cfg.local_identity = Some(LocalIdentity {
        irk: Secret::new([0xA5; 16]),
        address: CENTRAL_ADDR,
    });
    cfg
}

pub fn peripheral(io: IoCapability) -> Peripheral {
    Peripheral::new(io, PERIPHERAL_ADDR, CENTRAL_ADDR)
}

pub struct Run {
    pub result: Result<Bond, Error>,
    pub shown_passkey: Option<u32>,
    pub number: Option<u32>,
    /// Every PDU the central sent, before tampering.
    pub sent: Vec<Vec<u8>>,
    pub peripheral: Peripheral,
}

type Tamper = Box<dyn FnMut(Dir, &mut Vec<u8>)>;

pub struct Hooks {
    /// May rewrite a PDU in flight.
    pub tamper: Tamper,
    /// What the peer user types for the passkey the host showed.
    pub type_passkey: Box<dyn Fn(u32) -> u32>,
    /// The host user's answer to a Numeric Comparison.
    pub confirm: bool,
}

impl Hooks {
    /// A faithful link and a user who types the right passkey and agrees.
    pub fn plain() -> Hooks {
        Hooks {
            tamper: Box::new(|_, _| {}),
            type_passkey: Box::new(|p| p),
            confirm: true,
        }
    }

    pub fn tamper(mut self, f: impl FnMut(Dir, &mut Vec<u8>) + 'static) -> Hooks {
        self.tamper = Box::new(f);
        self
    }
}

/// Drive a whole pairing.
pub fn run(cfg: Config, mut per: Peripheral, mut hooks: Hooks) -> Run {
    let mut central = Central::new(cfg, seeded(b"central"));
    let mut to_central: VecDeque<Vec<u8>> = VecDeque::new();
    let mut shown_passkey = None;
    let mut number = None;
    let mut sent = Vec::new();
    let mut result = Err(Error::Timeout);
    let mut step = central.step(0, Input::Start);
    let mut now = 0u64;
    let mut user_answer: Option<bool> = None;
    let mut encrypt = false;
    for _ in 0..500 {
        for notice in &step.notices {
            match notice {
                Notice::DisplayPasskey(p) => {
                    shown_passkey = Some(*p);
                    per.typed = Some((hooks.type_passkey)(*p));
                }
                Notice::ConfirmNumber(n) => {
                    number = Some(*n);
                    user_answer = Some(hooks.confirm);
                }
                Notice::StartEncryption { ltk, .. } => {
                    assert_eq!(Some(*ltk.expose()), per.ltk, "both sides derive one LTK");
                    encrypt = true;
                }
            }
        }
        for mut pdu in step.send.drain(..) {
            sent.push(pdu.clone());
            (hooks.tamper)(Dir::ToPeripheral, &mut pdu);
            match per.on_pdu(&pdu) {
                Ok(replies) => {
                    for mut reply in replies {
                        (hooks.tamper)(Dir::ToCentral, &mut reply);
                        to_central.push_back(reply);
                    }
                }
                Err(reason) => to_central.push_back(Pdu::PairingFailed(reason).encode()),
            }
        }
        if let Some(outcome) = step.outcome.take() {
            result = outcome;
            break;
        }
        now += 10;
        step = if let Some(answer) = user_answer.take() {
            central.step(now, Input::UserConfirm(answer))
        } else if encrypt {
            encrypt = false;
            to_central.extend(per.on_encrypted());
            central.step(now, Input::Encryption { ok: true })
        } else if let Some(msg) = to_central.pop_front() {
            central.step(now, Input::Pdu(&msg))
        } else {
            panic!("stalled: nothing to deliver");
        };
    }
    Run {
        result,
        shown_passkey,
        number,
        sent,
        peripheral: per,
    }
}

/// `IoCapability` used for a mouse-like peer.
pub const MOUSE: IoCapability = IoCapability::NoInputNoOutput;
/// A keyboard-like peer.
pub const KEYBOARD: IoCapability = IoCapability::KeyboardOnly;
