//! The pre-shared-key handshake.
//!
//! The server opens with a random nonce (a `hello` notification). The client
//! proves it holds the key with `HMAC-SHA256(key, "lazyos-dbg/1 client" ||
//! nonce)`; the server answers with `HMAC-SHA256(key, "lazyos-dbg/1 server"
//! || nonce)`, so the client knows the box is the one it flashed. Nothing
//! secret crosses the wire, and a recorded handshake is useless against a
//! new nonce. There is no encryption in v1 (TLS comes with `nettls`); the
//! log it carries is the log a person standing at the machine can read.

use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::hmac;

/// The protocol name and version, in the `hello` and in both MAC inputs.
pub const PROTO: &str = "lazyos-dbg/1";
/// Nonce length in bytes.
pub const NONCE_LEN: usize = 16;
/// Shortest and longest accepted key (bytes): 128 bits and up.
pub const KEY_MIN: usize = 16;
pub const KEY_MAX: usize = 64;

const CLIENT_LABEL: &[u8] = b"lazyos-dbg/1 client";
const SERVER_LABEL: &[u8] = b"lazyos-dbg/1 server";

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    lazyos_crypto::hex::encode(bytes)
}

/// Bytes of a hex string (either case); `None` for odd length or a bad digit.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if text.len() % 2 != 0 {
        return None;
    }
    text.chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some((high << 4 | low) as u8)
        })
        .collect()
}

/// The key a `diag.dbg.key` value names: hex, [`KEY_MIN`]..=[`KEY_MAX`] bytes.
pub fn parse_key(text: &str) -> Option<Vec<u8>> {
    let key = unhex(text.trim())?;
    (KEY_MIN..=KEY_MAX).contains(&key.len()).then_some(key)
}

/// What the client sends in `auth`.
pub fn client_mac(key: &[u8], nonce: &[u8]) -> [u8; hmac::TAG_LEN] {
    let mut out = [0u8; hmac::TAG_LEN];
    hmac::hmac_sha256_parts(key, &[CLIENT_LABEL, nonce], &mut out);
    out
}

/// What the server answers with.
pub fn server_mac(key: &[u8], nonce: &[u8]) -> [u8; hmac::TAG_LEN] {
    let mut out = [0u8; hmac::TAG_LEN];
    hmac::hmac_sha256_parts(key, &[SERVER_LABEL, nonce], &mut out);
    out
}

/// Whether `mac_hex` is the client's proof for `nonce`, in constant time.
pub fn verify_client(key: &[u8], nonce: &[u8], mac_hex: &str) -> bool {
    match unhex(mac_hex) {
        Some(tag) => hmac::hmac_sha256_verify(key, &[CLIENT_LABEL, nonce], &tag),
        None => false,
    }
}

/// Failed attempts slow the next one down: 1 s, 2 s, 4 s ... up to a
/// minute, reset by a success. Times are in milliseconds from any clock that
/// does not go backwards.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lockout {
    failures: u32,
    until_ms: u64,
}

impl Lockout {
    pub const MAX_DELAY_MS: u64 = 60_000;

    pub const fn new() -> Lockout {
        Lockout {
            failures: 0,
            until_ms: 0,
        }
    }

    /// Whether an attempt may be judged at `now_ms`; otherwise how many
    /// milliseconds remain.
    pub fn check(&self, now_ms: u64) -> Result<(), u64> {
        if now_ms >= self.until_ms {
            Ok(())
        } else {
            Err(self.until_ms - now_ms)
        }
    }

    /// Record a failed attempt at `now_ms`.
    pub fn failed(&mut self, now_ms: u64) {
        self.failures = self.failures.saturating_add(1);
        let shift = (self.failures - 1).min(16);
        let delay = (1000u64 << shift).min(Self::MAX_DELAY_MS);
        self.until_ms = now_ms.saturating_add(delay);
    }

    pub fn succeeded(&mut self) {
        *self = Lockout::new();
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}
