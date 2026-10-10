//! The sealed file the secrets persist in.
//!
//! ```text
//! "LZSECRT1"                      8 bytes, magic and version
//! count                           u16 LE
//! count x { len u32 LE, blob }    one record each, `blob` = wrap(machine key, record)
//! tag                             32 bytes, HMAC-SHA256 of everything above
//! ```
//!
//! A record is `kind u8 (0 system, 1 user) | uid u32 LE | name_len u8 | name |
//! secret_len u16 LE | secret | n u8 | n x { ssid_len u8 | ssid | pmk 32 }`:
//! the secret and its cached PMKs, sealed with [`lazyos_crypto::wrap`] under
//! the machine key. The trailing tag (keyed by a subkey of the machine key)
//! covers the whole file, so a record cannot be removed, reordered, cut off
//! or flipped without notice; a file is accepted whole or refused whole.
//!
//! What this protects: a copy of the file *without* the machine key file. It
//! does not protect against anyone who can read both (root on the volume);
//! there is no TPM to hold the key (docs/security-model.md section 8).

use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::hmac::{hmac_sha256, hmac_sha256_verify, TAG_LEN};
use lazyos_crypto::wifi::PMK_LEN;
use lazyos_crypto::wrap::{self, NONCE_LEN};

use crate::{
    valid_name, valid_secret, Entry, Owner, Store, MAX_NAME, MAX_PER_OWNER, MAX_PMKS, MAX_SECRET,
    MAX_SSID, MAX_TOTAL,
};

/// Length of the machine key.
pub const MACHINE_KEY_LEN: usize = 32;
const MAGIC: &[u8; 8] = b"LZSECRT1";
/// Domain separation: the file tag's key is not the record wrapping key.
const TAG_LABEL: &[u8] = b"lazyos keyd secrets file v1";
const HEADER: usize = MAGIC.len() + 2;
/// The most a record's plaintext holds.
const MAX_RECORD: usize =
    1 + 4 + 1 + MAX_NAME + 2 + MAX_SECRET + 1 + MAX_PMKS * (1 + MAX_SSID + PMK_LEN);
/// The most a valid file holds; `keyd` reads no more than this.
pub const FILE_MAX: usize = HEADER + MAX_TOTAL * (4 + wrap::OVERHEAD + MAX_RECORD) + TAG_LEN;

/// Why a file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileError {
    /// Shorter than an empty file, or a field runs past the end.
    Truncated,
    /// Not a file of this format.
    BadMagic,
    /// The tag does not verify: damaged, or sealed under another key.
    BadTag,
    /// A record is not what the writer produces (cannot happen after the tag
    /// verified, unless the writer is broken).
    BadRecord,
    /// More records than the caps allow, or a duplicate.
    TooMany,
}

impl core::fmt::Display for FileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            FileError::Truncated => "truncated",
            FileError::BadMagic => "not a secrets file",
            FileError::BadTag => "damaged, or sealed under another machine key",
            FileError::BadRecord => "a record is malformed",
            FileError::TooMany => "over the caps",
        })
    }
}

fn tag_key(machine_key: &[u8; MACHINE_KEY_LEN]) -> [u8; TAG_LEN] {
    hmac_sha256(machine_key, TAG_LABEL)
}

impl Store {
    /// The file's bytes. `nonce` fills each record's fresh wrapping nonce
    /// (live callers draw it from the entropy pool; a repeated nonce under
    /// one key leaks the XOR of two plaintexts).
    pub fn seal(
        &self,
        machine_key: &[u8; MACHINE_KEY_LEN],
        nonce: &mut dyn FnMut(&mut [u8; NONCE_LEN]),
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        for entry in &self.entries {
            let mut fresh = [0u8; NONCE_LEN];
            nonce(&mut fresh);
            // The key is 32 bytes, so `wrap` cannot refuse it.
            let blob = wrap::wrap_with_nonce(machine_key, &fresh, &encode_record(entry))
                .expect("a 32-byte key wraps");
            out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            out.extend_from_slice(&blob);
        }
        let tag = hmac_sha256(&tag_key(machine_key), &out);
        out.extend_from_slice(&tag);
        out
    }

    /// Read a file back. All or nothing: the first thing wrong refuses it.
    pub fn open(bytes: &[u8], machine_key: &[u8; MACHINE_KEY_LEN]) -> Result<Store, FileError> {
        if bytes.len() < HEADER + TAG_LEN {
            return Err(FileError::Truncated);
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(FileError::BadMagic);
        }
        let (body, tag) = bytes.split_at(bytes.len() - TAG_LEN);
        if !hmac_sha256_verify(&tag_key(machine_key), &[body], tag) {
            return Err(FileError::BadTag);
        }
        let count = u16::from_le_bytes([body[MAGIC.len()], body[MAGIC.len() + 1]]) as usize;
        if count > MAX_TOTAL {
            return Err(FileError::TooMany);
        }
        let mut rest = &body[HEADER..];
        let mut store = Store::new();
        for _ in 0..count {
            let len = take(&mut rest, 4)?;
            let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
            let blob = take(&mut rest, len)?;
            let plain = wrap::unwrap(machine_key, blob).map_err(|_| FileError::BadTag)?;
            let entry = decode_record(&plain)?;
            let held = store
                .entries
                .iter()
                .filter(|e| e.owner == entry.owner)
                .count();
            if held >= MAX_PER_OWNER
                || store
                    .entries
                    .iter()
                    .any(|e| e.owner == entry.owner && e.name == entry.name)
            {
                return Err(FileError::TooMany);
            }
            store.entries.push(entry);
        }
        if !rest.is_empty() {
            return Err(FileError::BadRecord);
        }
        Ok(store)
    }
}

fn encode_record(entry: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    match entry.owner {
        Owner::System => out.extend_from_slice(&[0, 0, 0, 0, 0]),
        Owner::User(uid) => {
            out.push(1);
            out.extend_from_slice(&uid.to_le_bytes());
        }
    }
    out.push(entry.name.len() as u8);
    out.extend_from_slice(entry.name.as_bytes());
    out.extend_from_slice(&(entry.secret.len() as u16).to_le_bytes());
    out.extend_from_slice(&entry.secret);
    out.push(entry.pmks.len() as u8);
    for (ssid, pmk) in &entry.pmks {
        out.push(ssid.len() as u8);
        out.extend_from_slice(ssid);
        out.extend_from_slice(pmk);
    }
    out
}

/// The next `len` bytes of `rest`, advancing it.
fn take<'a>(rest: &mut &'a [u8], len: usize) -> Result<&'a [u8], FileError> {
    if rest.len() < len {
        return Err(FileError::Truncated);
    }
    let (head, tail) = rest.split_at(len);
    *rest = tail;
    Ok(head)
}

fn decode_record(plain: &[u8]) -> Result<Entry, FileError> {
    let mut rest = plain;
    let head = take(&mut rest, 5)?;
    let uid = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    let owner = match (head[0], uid) {
        (0, 0) => Owner::System,
        (1, uid) => Owner::User(uid),
        _ => return Err(FileError::BadRecord),
    };
    let name_len = take(&mut rest, 1)?[0] as usize;
    let name =
        core::str::from_utf8(take(&mut rest, name_len)?).map_err(|_| FileError::BadRecord)?;
    let secret_len = take(&mut rest, 2)?;
    let secret_len = u16::from_le_bytes([secret_len[0], secret_len[1]]) as usize;
    let secret = take(&mut rest, secret_len)?;
    if !valid_name(name) || !valid_secret(secret) {
        return Err(FileError::BadRecord);
    }
    let count = take(&mut rest, 1)?[0] as usize;
    if count > MAX_PMKS {
        return Err(FileError::BadRecord);
    }
    let mut pmks = Vec::new();
    for _ in 0..count {
        let ssid_len = take(&mut rest, 1)?[0] as usize;
        if !(1..=MAX_SSID).contains(&ssid_len) {
            return Err(FileError::BadRecord);
        }
        let ssid = take(&mut rest, ssid_len)?.to_vec();
        let pmk: [u8; PMK_LEN] = take(&mut rest, PMK_LEN)?
            .try_into()
            .map_err(|_| FileError::BadRecord)?;
        pmks.push((ssid, pmk));
    }
    if !rest.is_empty() {
        return Err(FileError::BadRecord);
    }
    Ok(Entry {
        owner,
        name: String::from(name),
        secret: secret.to_vec(),
        pmks,
    })
}
