//! `keyd`'s named secrets (docs/wifi-prerequisites-plan.md WP2, section 3.3).
//!
//! A *secret* is bytes a person chose (a Wi-Fi passphrase), kept under a name
//! in one of two scopes:
//!
//! * `user`: the caller's own, keyed by the kernel-stamped uid. Any caller may
//!   store, delete and list its own.
//! * `system`: available to every user and before anyone logs in. Stored and
//!   deleted only on `elevd`'s say-so (an administrator typed their password
//!   on the trusted prompt); the caller is identified by `elevd`'s system
//!   uid, never by uid 0 or a capability.
//!
//! No call returns a secret. The one use is [`Store::pmk`]: the WPA2 pairwise
//! master key of a passphrase for an SSID, which only `wlanmd` (the
//! [`WLAN_UID`] service) may ask for ([`authorize`]). The PBKDF2 behind it is
//! 8192 SHA-1 compressions per output block, so the result is cached beside
//! the secret and computed once per (secret, SSID).
//!
//! This crate holds everything that can be tested without a machine: the rules
//! ([`valid_name`], [`valid_secret`], the caps), who may do what
//! ([`authorize`]), the table ([`Store`]) and the sealed file it persists in
//! ([`Store::seal`], [`Store::open`], `file.rs`). `keyd` adds the transport
//! and the disk.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;

pub use accountdb::ELEVD_UID;
use lazyos_crypto::wifi::{self, PMK_LEN};

mod file;
mod rules;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_file;

pub use file::{FileError, FILE_MAX, MACHINE_KEY_LEN};
pub use rules::{authorize, Caller, Denied, Op};

/// The `_wlan` system uid `wlanmd` runs as (reserved in `libs/devmatch`'s
/// uid list, docs/wifi-prerequisites-plan.md section 3.1): the one identity
/// [`Store::pmk`] is answered to.
pub const WLAN_UID: u32 = 912;
/// Longest secret name.
pub const MAX_NAME: usize = 64;
/// Longest secret. A passphrase is at most 63 characters and a raw PSK 64 hex
/// digits; the rest is room for other credentials, still far below the wire's
/// 8 KiB cap.
pub const MAX_SECRET: usize = 256;
/// Secrets one owner (a uid, or the system) may hold, so one user cannot fill
/// the table and lock everyone out.
pub const MAX_PER_OWNER: usize = 16;
/// Secrets in all.
pub const MAX_TOTAL: usize = 64;
/// SSIDs whose PMK is cached per secret; the oldest is dropped for a new one.
pub const MAX_PMKS: usize = 8;
/// Longest SSID, in octets (802.11).
pub const MAX_SSID: usize = 32;

/// Whose secret it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Available to every user and before login (`elevd` stored it).
    System,
    /// One user's, by uid.
    User(u32),
}

/// Why a [`Store`] call failed. `keyd` maps these onto errno codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The name breaks [`valid_name`].
    BadName,
    /// The secret breaks [`valid_secret`], or is not a valid passphrase for a
    /// PMK.
    BadSecret,
    /// The SSID is not 1..=32 octets.
    BadSsid,
    /// The owner or the table is at its cap.
    Full,
    /// No such secret for that owner.
    NotFound,
}

/// A name a secret may have: 1..=[`MAX_NAME`] of letters, digits and
/// `. _ - :`. It is shown in lists and in the trusted prompt, so nothing that
/// can hide or fake text is allowed in it.
pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:".contains(&byte))
}

/// A secret `keyd` will keep: 1..=[`MAX_SECRET`] bytes.
pub fn valid_secret(secret: &[u8]) -> bool {
    (1..=MAX_SECRET).contains(&secret.len())
}

#[derive(Clone)]
struct Entry {
    owner: Owner,
    name: String,
    secret: Vec<u8>,
    /// (SSID, PMK) pairs, oldest first.
    pmks: Vec<(Vec<u8>, [u8; PMK_LEN])>,
}

/// The table of named secrets.
#[derive(Clone, Default)]
pub struct Store {
    entries: Vec<Entry>,
}

impl Store {
    /// An empty table.
    pub fn new() -> Store {
        Store::default()
    }

    /// How many secrets are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, owner: Owner, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.owner == owner && entry.name == name)
    }

    /// Keep `secret` as `owner`'s `name`, replacing one of that name (and
    /// dropping the PMKs derived from it).
    pub fn put(&mut self, owner: Owner, name: &str, secret: &[u8]) -> Result<(), Error> {
        if !valid_name(name) {
            return Err(Error::BadName);
        }
        if !valid_secret(secret) {
            return Err(Error::BadSecret);
        }
        let entry = Entry {
            owner,
            name: String::from(name),
            secret: secret.to_vec(),
            pmks: Vec::new(),
        };
        match self.position(owner, name) {
            Some(index) => self.entries[index] = entry,
            None => {
                let held = self.entries.iter().filter(|e| e.owner == owner).count();
                if held >= MAX_PER_OWNER || self.entries.len() >= MAX_TOTAL {
                    return Err(Error::Full);
                }
                self.entries.push(entry);
            }
        }
        Ok(())
    }

    /// Forget `owner`'s `name`.
    pub fn delete(&mut self, owner: Owner, name: &str) -> Result<(), Error> {
        let index = self.position(owner, name).ok_or(Error::NotFound)?;
        self.entries.remove(index);
        Ok(())
    }

    /// `owner`'s secret names, sorted. Never the secrets.
    pub fn names(&self, owner: Owner) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| entry.owner == owner)
            .map(|entry| entry.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The WPA2 PMK of `owner`'s `name` for `ssid`, and whether it was just
    /// computed (so the caller persists the cache). A 64-hex-digit secret is
    /// a raw PSK, which already is the PMK; anything else must be a
    /// passphrase (8..=63 printable ASCII).
    pub fn pmk(
        &mut self,
        owner: Owner,
        name: &str,
        ssid: &[u8],
    ) -> Result<([u8; PMK_LEN], bool), Error> {
        let index = self.position(owner, name).ok_or(Error::NotFound)?;
        if !(1..=MAX_SSID).contains(&ssid.len()) {
            return Err(Error::BadSsid);
        }
        let entry = &mut self.entries[index];
        if let Some(raw) = raw_psk(&entry.secret) {
            return Ok((raw, false));
        }
        if let Some((_, pmk)) = entry.pmks.iter().find(|(known, _)| known == ssid) {
            return Ok((*pmk, false));
        }
        let pmk = wifi::pbkdf2_sha1(&entry.secret, ssid).map_err(|_| Error::BadSecret)?;
        if entry.pmks.len() >= MAX_PMKS {
            entry.pmks.remove(0);
        }
        entry.pmks.push((ssid.to_vec(), pmk));
        Ok((pmk, true))
    }

    /// How many PMKs are cached for `owner`'s `name` (tests, diagnostics).
    pub fn cached(&self, owner: Owner, name: &str) -> Option<usize> {
        self.position(owner, name)
            .map(|index| self.entries[index].pmks.len())
    }
}

/// The 32 bytes a 64-hex-digit secret spells, if it is one. (A passphrase is
/// at most 63 characters, so the two never overlap.)
fn raw_psk(secret: &[u8]) -> Option<[u8; PMK_LEN]> {
    if secret.len() != PMK_LEN * 2 {
        return None;
    }
    let digit = |byte: u8| (byte as char).to_digit(16).map(|value| value as u8);
    let mut out = [0u8; PMK_LEN];
    for (slot, pair) in out.iter_mut().zip(secret.chunks(2)) {
        *slot = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(out)
}
