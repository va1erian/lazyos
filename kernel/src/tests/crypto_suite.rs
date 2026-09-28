//! Runs the same primitives `keyd` links in ring 3 inside the kernel,
//! plus the key-material isolation property: a `SHARE_ONLY` buffer is
//! mapped for its creator and the kernel refuses every other task's
//! `map`. keyd crypto and SHARE_ONLY key isolation (issue #102).

use super::*;
use crate::ipc::channels;
use crate::ipc::handles::{self, Error as HandleError};
use crate::ipc::shared::{self, Error as BufferError};
use alloc::vec;
use lazyos_crypto::{hex, hmac, sha256, wrap};

/// Friendly text for a crypto failure.
fn crypto_reason(error: lazyos_crypto::Error) -> String {
    error.message().into()
}

/// FIPS 180-4 and RFC 4231 vectors, run on the freestanding target so the
/// exact artifact `keyd` embeds is covered, not just the host build.
pub fn sha256_hmac_known_answers() -> Result<(), String> {
    let digest = hex::encode(&sha256::sha256(b"abc"));
    check!(
        digest == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "sha256(abc) = {digest}"
    );
    let empty = hex::encode(&sha256::sha256(b""));
    check!(
        empty == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "sha256(\"\") = {empty}"
    );
    let tag = hex::encode(&hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There"));
    check!(
        tag == "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "hmac(key, \"Hi There\") = {tag}"
    );
    check!(
        hmac::hmac_sha256_verify(
            &[0x0bu8; 20],
            &[b"Hi There"],
            &hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There")
        ),
        "the constant-time tag check rejected a valid tag"
    );
    Ok(())
}

/// A wrap->unwrap round-trip in kernel context, then the same blob handed
/// to a client inside a `SHARE_ONLY` buffer: the service reads and unwraps
/// its own mapping first, and the client receives the handle afterwards but
/// cannot map it.
pub fn keyd_wrap_roundtrip_share_only() -> Result<(), String> {
    // Part 1: the wrapper round-trips and refuses tampering.
    let key = [0x42u8; 32];
    let nonce = [0x24u8; wrap::NONCE_LEN];
    let secret = b"launch codes: 0000";
    let blob = wrap::wrap_with_nonce(&key, &nonce, secret).map_err(crypto_reason)?;
    let opened = wrap::unwrap(&key, &blob).map_err(crypto_reason)?;
    check!(opened == secret, "wrap round-trip mismatch");
    let mut tampered = blob.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    check!(
        wrap::unwrap(&key, &tampered) == Err(lazyos_crypto::Error::BadTag),
        "a tampered blob unwrapped"
    );

    // Part 2: the SHARE_ONLY handoff. `fresh` mirrors
    // `buffer_share_only_not_mappable`: the registry starts empty.
    ipc_shared_suite::fresh()?;
    let creator = task::current();
    let child = ipc_shared_suite::spawn_receiver()?;
    let (client, child_server) = ipc_shared_suite::channel_to(child)?;
    let handle = shared::create(
        blob.len() as u64,
        shared::flags::READ | shared::flags::WRITE | shared::flags::SHARE_ONLY,
    )
    .map_err(ipc_shared_suite::buffer_reason)?;
    let creator_va = shared::map(handle).map_err(ipc_shared_suite::buffer_reason)?;
    // The creator (standing in for `keyd`) writes the wrapped blob through
    // its own mapping and can read it back: material at rest is visible
    // only to the service.
    for (offset, byte) in blob.iter().enumerate() {
        // Safety: the creator's mapping is writable for the buffer size.
        unsafe { (creator_va as *mut u8).add(offset).write_volatile(*byte) };
    }
    let mut readback = vec![0u8; blob.len()];
    for (offset, slot) in readback.iter_mut().enumerate() {
        // Safety: the creator's mapping is readable for the buffer size.
        *slot = unsafe { (creator_va as *const u8).add(offset).read_volatile() };
    }
    check!(readback == blob, "the service's own mapping changed");
    let opened = wrap::unwrap(&key, &readback).map_err(crypto_reason)?;
    check!(
        opened == secret,
        "the service could not unwrap its own blob"
    );

    // Hand the handle to the client. The transfer moves the handle and its
    // only mapping out of the creator; the client gets the handle but the
    // kernel refuses to map it, so no client address space ever sees the
    // blob.
    let bytes =
        ipc_shared_suite::parcel_with_transfers(1, "wrapped key", vec![handle], Vec::new())?;
    channels::send(client, &bytes).map_err(ipc_shared_suite::channel_reason)?;
    check!(
        handles::get(handle) == Err(HandleError::InvalidHandle),
        "the transfer did not move the sender's handle"
    );
    check!(
        raw_entry(mem::kernel_table(), creator_va).is_none(),
        "the creator's mapping outlived the handle transfer"
    );

    task::harness::switch_current(child);
    let message = channels::try_recv(child_server)
        .map_err(ipc_shared_suite::channel_reason)?
        .ok_or("the transferred message is missing")?;
    check!(
        message.handles.len() == 1,
        "delivered {} handles, expected 1",
        message.handles.len()
    );
    check!(
        shared::map(message.handles[0]) == Err(BufferError::ShareOnly),
        "a client mapped a SHARE_ONLY key buffer"
    );

    // Cleanup in the same order as the other shared-buffer tests: the
    // registries go first so the queued message's references are released.
    shared::reset();
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    channels::reset();
    ipc_shared_suite::reap(child)?;
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("keyd_sha256_hmac_known_answers", sha256_hmac_known_answers),
    (
        "keyd_wrap_roundtrip_share_only",
        keyd_wrap_roundtrip_share_only,
    ),
];
