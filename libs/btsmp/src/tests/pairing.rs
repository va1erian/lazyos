//! Full pairings between the real central and the test peripheral.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;

use lazyos_crypto::p256::KeyPair;

use super::harness::*;
use super::peripheral::Peripheral;
use crate::pairing::{Central, Input, Notice};
use crate::pdu::{AddrType, IoCapability, Pdu, Reason};
use crate::{Error, Method};

fn err(run: &Run) -> Error {
    *run.result.as_ref().unwrap_err()
}

fn last_sent(run: &Run) -> Pdu {
    Pdu::decode(run.sent.last().unwrap()).unwrap()
}

/// Flip one bit of the first PDU going `dir` whose code is `code`.
fn flip(dir: Dir, code: u8, byte: usize) -> impl FnMut(Dir, &mut Vec<u8>) {
    let mut done = false;
    move |d, pdu| {
        if !done && d == dir && pdu[0] == code {
            pdu[byte] ^= 1;
            done = true;
        }
    }
}

#[test]
fn just_works_pairs_and_bonds() {
    let run = run(config(), peripheral(MOUSE), Hooks::plain());
    let bond = run.result.expect("pairing succeeds");
    assert_eq!(bond.method, Method::JustWorks);
    assert!(!bond.authenticated, "Just Works is not MITM protected");
    assert_eq!(bond.key_size, 16);
    assert_eq!(Some(*bond.ltk.expose()), run.peripheral.ltk);
    assert_eq!(
        bond.peer_irk.as_ref().map(|k| *k.expose()),
        Some([0x5A; 16])
    );
    assert_eq!(bond.peer_identity, Some(PERIPHERAL_ADDR));
    assert_eq!(bond.peer_address, PERIPHERAL_ADDR);
    // Our identity went to the peer after its own.
    assert_eq!(run.peripheral.got_irk, Some([0xA5; 16]));
    assert_eq!(run.peripheral.got_identity, Some(CENTRAL_ADDR));
    assert!(run.shown_passkey.is_none());
}

#[test]
fn passkey_entry_pairs_with_an_authenticated_key() {
    let mut cfg = config();
    cfg.require_authenticated = true;
    let run = run(cfg, peripheral(KEYBOARD), Hooks::plain());
    let bond = run.result.expect("pairing succeeds");
    assert_eq!(bond.method, Method::PasskeyEntry);
    assert!(bond.authenticated);
    assert_eq!(Some(*bond.ltk.expose()), run.peripheral.ltk);
    assert!(run.shown_passkey.unwrap() < 1_000_000);
    // 20 commitment rounds: 20 confirms and 20 randoms from the central.
    let count = |code| run.sent.iter().filter(|p| p[0] == code).count();
    assert_eq!(count(crate::pdu::code::PAIRING_CONFIRM), 20);
    assert_eq!(count(crate::pdu::code::PAIRING_RANDOM), 20);
}

#[test]
fn passkey_entry_without_our_identity() {
    let mut cfg = config();
    cfg.local_identity = None;
    let run = run(cfg, peripheral(KEYBOARD), Hooks::plain());
    run.result.expect("pairing succeeds");
    assert_eq!(run.peripheral.got_irk, None);
}

#[test]
fn peer_without_identity_keys_still_bonds() {
    let mut per = peripheral(MOUSE);
    per.send_identity = false;
    let bond = run(config(), per, Hooks::plain()).result.unwrap();
    assert!(bond.peer_irk.is_none() && bond.peer_identity.is_none());
}

#[test]
fn numeric_comparison_shows_the_same_number_on_both_sides() {
    let run = run(
        config(),
        peripheral(IoCapability::DisplayYesNo),
        Hooks::plain(),
    );
    let bond = run.result.expect("pairing succeeds");
    assert_eq!(bond.method, Method::NumericComparison);
    assert!(bond.authenticated);
    assert_eq!(run.number, run.peripheral.number);
    assert!(run.number.unwrap() < 1_000_000);
}

#[test]
fn numeric_comparison_rejected_by_the_user() {
    let mut hooks = Hooks::plain();
    hooks.confirm = false;
    let run = run(config(), peripheral(IoCapability::DisplayYesNo), hooks);
    assert_eq!(err(&run), Error::UserRejected);
    assert_eq!(
        last_sent(&run),
        Pdu::PairingFailed(Reason::NUMERIC_COMPARISON_FAILED)
    );
}

#[test]
fn wrong_passkey_is_caught_by_the_peer() {
    let mut hooks = Hooks::plain();
    // The user mistypes: bit 7 differs, so round 7 fails.
    hooks.type_passkey = alloc::boxed::Box::new(|p| p ^ (1 << 7));
    let run = run(config(), peripheral(KEYBOARD), hooks);
    assert_eq!(err(&run), Error::PeerFailed(Reason::CONFIRM_VALUE_FAILED));
    let confirms = run
        .sent
        .iter()
        .filter(|p| p[0] == crate::pdu::code::PAIRING_CONFIRM)
        .count();
    assert_eq!(
        confirms, 8,
        "fails in round 7, never reaches the DHKey check"
    );
}

#[test]
fn wrong_passkey_is_caught_by_the_central_when_the_peer_does_not_check() {
    let mut hooks = Hooks::plain();
    hooks.type_passkey = alloc::boxed::Box::new(|p| p ^ 1);
    let mut per = peripheral(KEYBOARD);
    per.verify_ca = false;
    let run = run(config(), per, hooks);
    assert_eq!(err(&run), Error::ConfirmMismatch);
    assert_eq!(
        last_sent(&run),
        Pdu::PairingFailed(Reason::CONFIRM_VALUE_FAILED)
    );
}

#[test]
fn confirm_mismatch_just_works() {
    use crate::pdu::code::PAIRING_CONFIRM;
    let hooks = Hooks::plain().tamper(flip(Dir::ToCentral, PAIRING_CONFIRM, 3));
    let run = run(config(), peripheral(MOUSE), hooks);
    assert_eq!(err(&run), Error::ConfirmMismatch);
    assert_eq!(
        last_sent(&run),
        Pdu::PairingFailed(Reason::CONFIRM_VALUE_FAILED)
    );
}

#[test]
fn dhkey_check_mismatch() {
    use crate::pdu::code::DHKEY_CHECK;
    let hooks = Hooks::plain().tamper(flip(Dir::ToCentral, DHKEY_CHECK, 5));
    let run = run(config(), peripheral(MOUSE), hooks);
    assert_eq!(err(&run), Error::DhKeyCheckMismatch);
    assert_eq!(
        last_sent(&run),
        Pdu::PairingFailed(Reason::DHKEY_CHECK_FAILED)
    );
}

#[test]
fn a_man_in_the_middle_swapping_the_public_key_is_caught() {
    // Replace the peripheral's key by a valid one of the attacker's: the
    // confirm and check values no longer agree.
    let attacker = KeyPair::from_secret_be(&[7u8; 32]).unwrap().public_smp();
    let hooks = Hooks::plain().tamper(move |d, pdu| {
        if d == Dir::ToCentral && pdu[0] == crate::pdu::code::PUBLIC_KEY {
            pdu[1..].copy_from_slice(&attacker);
        }
    });
    let run = run(config(), peripheral(MOUSE), hooks);
    assert_eq!(err(&run), Error::ConfirmMismatch);
}

#[test]
fn invalid_curve_point_is_refused() {
    // CVE-2018-5383: one bit off the curve.
    let hooks = Hooks::plain().tamper(flip(Dir::ToCentral, crate::pdu::code::PUBLIC_KEY, 40));
    let run = run(config(), peripheral(MOUSE), hooks);
    assert_eq!(err(&run), Error::InvalidPublicKey);
    assert_eq!(
        last_sent(&run),
        Pdu::PairingFailed(Reason::INVALID_PARAMETERS)
    );
}

#[test]
fn the_debug_key_is_refused() {
    let debug_private = super::vectors::hex::<32>(
        "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd",
    );
    let mut per = peripheral(MOUSE);
    per.set_keypair(KeyPair::from_secret_be(&debug_private).unwrap());
    let run = run(config(), per, Hooks::plain());
    assert_eq!(err(&run), Error::WeakPublicKey);
}

#[test]
fn our_own_key_reflected_back_is_refused() {
    let mut ours = None;
    let hooks = Hooks::plain().tamper(move |d, pdu| match (d, pdu[0]) {
        (Dir::ToPeripheral, crate::pdu::code::PUBLIC_KEY) => ours = Some(pdu[1..].to_vec()),
        (Dir::ToCentral, crate::pdu::code::PUBLIC_KEY) => {
            pdu[1..].copy_from_slice(ours.as_ref().unwrap())
        }
        _ => {}
    });
    let run = run(config(), peripheral(MOUSE), hooks);
    assert_eq!(err(&run), Error::WeakPublicKey);
}

#[test]
fn legacy_pairing_is_refused() {
    let mut per = peripheral(MOUSE);
    per.legacy = true;
    let run = run(config(), per, Hooks::plain());
    assert_eq!(err(&run), Error::LegacyPairingRefused);
    assert_eq!(
        last_sent(&run),
        Pdu::PairingFailed(Reason::PAIRING_NOT_SUPPORTED)
    );
}

#[test]
fn short_keys_are_refused_by_default_and_masked_when_allowed() {
    let mut per = peripheral(MOUSE);
    per.max_key_size = 7;
    let run1 = run(config(), per, Hooks::plain());
    assert_eq!(err(&run1), Error::KeySizeTooSmall { size: 7 });
    assert_eq!(
        last_sent(&run1),
        Pdu::PairingFailed(Reason::ENCRYPTION_KEY_SIZE)
    );

    let mut cfg = config();
    cfg.min_key_size = 7;
    let mut per = peripheral(MOUSE);
    per.max_key_size = 7;
    let run2 = run(cfg, per, Hooks::plain());
    let bond = run2.result.unwrap();
    assert_eq!(bond.key_size, 7);
    assert!(bond.ltk.expose()[7..].iter().all(|b| *b == 0));
    assert_eq!(Some(*bond.ltk.expose()), run2.peripheral.ltk);
}

#[test]
fn an_authenticated_key_can_be_required() {
    let mut cfg = config();
    cfg.require_authenticated = true;
    let run = run(cfg, peripheral(MOUSE), Hooks::plain());
    assert_eq!(err(&run), Error::AuthenticationRequired);
    assert_eq!(
        last_sent(&run),
        Pdu::PairingFailed(Reason::AUTHENTICATION_REQUIREMENTS)
    );
}

#[test]
fn an_unseeded_rng_sends_nothing() {
    let mut central = Central::new(config(), lazyos_crypto::rng::Entropy::new());
    let step = central.step(0, Input::Start);
    assert!(step.send.is_empty());
    assert_eq!(step.outcome.unwrap().unwrap_err(), Error::Unseeded);
}

#[test]
fn the_smp_timeout_is_a_deadline_without_a_failed_pdu() {
    let mut central = Central::new(config(), seeded(b"t"));
    let step = central.step(1_000, Input::Start);
    assert_eq!(step.deadline_ms, Some(31_000));
    assert!(central.step(30_999, Input::Tick).outcome.is_none());
    let step = central.step(31_000, Input::Tick);
    assert_eq!(step.outcome.unwrap().unwrap_err(), Error::Timeout);
    assert!(step.send.is_empty(), "a timeout sends no Pairing Failed");
    assert!(!central.is_active());
    assert!(central.step(40_000, Input::Tick).outcome.is_none());
}

#[test]
fn ignorable_pdus_do_not_extend_the_deadline() {
    let mut central = Central::new(config(), seeded(b"t"));
    central.step(0, Input::Start);
    let keypress = Pdu::KeypressNotification(0).encode();
    let security = Pdu::SecurityRequest(crate::pdu::AuthReq(0x0D)).encode();
    for t in [10_000, 20_000, 29_000] {
        for pdu in [&keypress, &security] {
            let step = central.step(t, Input::Pdu(pdu));
            assert_eq!(step.deadline_ms, Some(30_000));
        }
    }
    let step = central.step(30_000, Input::Pdu(&keypress));
    assert_eq!(step.outcome.unwrap().unwrap_err(), Error::Timeout);
}

#[test]
fn hostile_pdus_end_the_pairing_cleanly() {
    let cases: [(&[u8], Error); 5] = [
        (&[], Error::Malformed(crate::pdu::DecodeError::Empty)),
        (
            &[0x7F, 0],
            Error::Malformed(crate::pdu::DecodeError::UnknownCode(0x7F)),
        ),
        (
            &[0x02, 0, 0],
            Error::Malformed(crate::pdu::DecodeError::BadLength { code: 2 }),
        ),
        // A response before... a Public Key while waiting for the response.
        (&[0x0C; 65], Error::UnexpectedPdu { code: 0x0C }),
        (
            &[0x05, 0x04],
            Error::PeerFailed(Reason::CONFIRM_VALUE_FAILED),
        ),
    ];
    for (bytes, want) in cases {
        let mut central = Central::new(config(), seeded(b"t"));
        central.step(0, Input::Start);
        let step = central.step(1, Input::Pdu(bytes));
        assert_eq!(step.outcome.unwrap().unwrap_err(), want, "{bytes:02x?}");
        assert!(central.step(2, Input::Pdu(bytes)).outcome.is_none());
    }
}

/// Run a Just Works pairing up to the point where the host must start
/// encryption.
fn to_encryption() -> (Central, Peripheral) {
    let mut central = Central::new(config(), seeded(b"central"));
    let mut per = peripheral(MOUSE);
    let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
    let mut step = central.step(0, Input::Start);
    loop {
        if step
            .notices
            .iter()
            .any(|n| matches!(n, Notice::StartEncryption { .. }))
        {
            return (central, per);
        }
        for pdu in step.send.drain(..) {
            queue.extend(per.on_pdu(&pdu).unwrap());
        }
        let msg = queue.pop_front().expect("stalled");
        step = central.step(1, Input::Pdu(&msg));
    }
}

#[test]
fn keys_before_encryption_are_not_believed() {
    // An attacker on the unencrypted link injects Identity Information.
    let (mut central, _) = to_encryption();
    let forged = Pdu::IdentityInfo([9; 16]).encode();
    let out = central.step(2, Input::Pdu(&forged));
    assert_eq!(
        out.outcome.unwrap().unwrap_err(),
        Error::UnexpectedPdu { code: 0x08 }
    );
}

#[test]
fn a_failed_encryption_fails_the_pairing() {
    let (mut central, _) = to_encryption();
    let out = central.step(2, Input::Encryption { ok: false });
    assert_eq!(out.outcome.unwrap().unwrap_err(), Error::EncryptionFailed);
}

#[test]
fn legacy_key_pdus_are_refused_after_encryption() {
    // LE SC derives the LTK; a peer sending one anyway is not following it.
    let (mut central, _) = to_encryption();
    assert!(central
        .step(2, Input::Encryption { ok: true })
        .outcome
        .is_none());
    let out = central.step(3, Input::Pdu(&Pdu::EncryptionInfo([1; 16]).encode()));
    assert_eq!(
        out.outcome.unwrap().unwrap_err(),
        Error::UnexpectedPdu { code: 0x06 }
    );
}

#[test]
fn a_private_identity_address_is_refused() {
    let mut per = peripheral(MOUSE);
    // Random, but a resolvable private address (top bits 01), not static.
    per.identity.addr = [1, 2, 3, 4, 5, 0x45];
    per.identity.kind = AddrType::Random;
    let run = run(config(), per, Hooks::plain());
    assert_eq!(err(&run), Error::InvalidIdentity);
}

#[test]
fn secrets_never_reach_debug_output() {
    let run = run(config(), peripheral(KEYBOARD), Hooks::plain());
    let passkey = run.shown_passkey.unwrap();
    let bond = run.result.unwrap();
    let ltk = *bond.ltk.expose();
    let text = format!("{bond:?}");
    assert!(text.contains("redacted"));
    assert!(!text.contains(&format!("{ltk:?}")));
    let notice = format!("{:?}", Notice::DisplayPasskey(passkey));
    assert!(!notice.contains(&format!("{passkey}")) || passkey < 10);
    let central = Central::new(config(), seeded(b"x"));
    assert_eq!(format!("{central:?}"), "Central(Idle)");
}
