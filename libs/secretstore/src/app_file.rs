//! The sealed file apps' secrets persist in (`appsecrets`).
//!
//! ```text
//! "LZAPSECR"                      8 bytes, magic
//! version                         u16 LE, [`APP_FILE_VERSION`]
//! count                           u16 LE
//! count x { len u32 LE, blob }    one record each, `blob` = wrap(machine key, record)
//! tag                             32 bytes, HMAC-SHA256 of everything above
//! ```
//!
//! A record is `uid u32 LE | app_len u8 | app | name_len u8 | name |
//! secret_len u16 LE | secret`. The tag covers the header too, so a version
//! cannot be altered unnoticed; a version this build does not know is refused
//! whole ([`FileError::BadVersion`]) and a future format bumps the number. The
//! named secrets' file (`secrets`, `file.rs`) is a different file with its own
//! magic and is not touched by any of this.
//!
//! As for the named secrets, this protects a copy of the file without the
//! machine key, not root on the volume (docs/security-model.md section 8).

use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::hmac::{hmac_sha256, hmac_sha256_verify, TAG_LEN};
use lazyos_crypto::wrap::{self, NONCE_LEN};

use crate::app::{AppEntry, AppStore};
use crate::file::{take, MACHINE_KEY_LEN};
use crate::{
    valid_app_name, valid_app_secret, FileError, APP_MAX_ID, APP_MAX_NAME, APP_MAX_PER_APP,
    APP_MAX_SECRET, APP_MAX_TOTAL,
};

const MAGIC: &[u8; 8] = b"LZAPSECR";
/// The format this build writes and reads.
pub const APP_FILE_VERSION: u16 = 1;
/// Domain separation: not the named secrets' tag key, nor the record key.
const TAG_LABEL: &[u8] = b"lazyos keyd appsecrets file";
const HEADER: usize = MAGIC.len() + 2 + 2;
const MAX_RECORD: usize = 4 + 1 + APP_MAX_ID + 1 + APP_MAX_NAME + 2 + APP_MAX_SECRET;
/// The most a valid file holds; `keyd` reads no more than this.
pub const APP_FILE_MAX: usize =
    HEADER + APP_MAX_TOTAL * (4 + wrap::OVERHEAD + MAX_RECORD) + TAG_LEN;

fn tag_key(machine_key: &[u8; MACHINE_KEY_LEN]) -> [u8; TAG_LEN] {
    hmac_sha256(machine_key, TAG_LABEL)
}

impl AppStore {
    /// The file's bytes. `nonce` fills each record's fresh wrapping nonce.
    pub fn seal(
        &self,
        machine_key: &[u8; MACHINE_KEY_LEN],
        nonce: &mut dyn FnMut(&mut [u8; NONCE_LEN]),
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&APP_FILE_VERSION.to_le_bytes());
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
    pub fn open(bytes: &[u8], machine_key: &[u8; MACHINE_KEY_LEN]) -> Result<AppStore, FileError> {
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
        let version = u16::from_le_bytes([body[MAGIC.len()], body[MAGIC.len() + 1]]);
        if version != APP_FILE_VERSION {
            return Err(FileError::BadVersion);
        }
        let count = u16::from_le_bytes([body[MAGIC.len() + 2], body[MAGIC.len() + 3]]) as usize;
        if count > APP_MAX_TOTAL {
            return Err(FileError::TooMany);
        }
        let mut rest = &body[HEADER..];
        let mut store = AppStore::new();
        for _ in 0..count {
            let len = take(&mut rest, 4)?;
            let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
            let blob = take(&mut rest, len)?;
            let plain = wrap::unwrap(machine_key, blob).map_err(|_| FileError::BadTag)?;
            let entry = decode_record(&plain)?;
            let held = store
                .entries
                .iter()
                .filter(|e| e.uid == entry.uid && e.app == entry.app)
                .count();
            let duplicate = store
                .entries
                .iter()
                .any(|e| e.uid == entry.uid && e.app == entry.app && e.name == entry.name);
            if held >= APP_MAX_PER_APP || duplicate {
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

fn encode_record(entry: &AppEntry) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&entry.uid.to_le_bytes());
    out.push(entry.app.len() as u8);
    out.extend_from_slice(entry.app.as_bytes());
    out.push(entry.name.len() as u8);
    out.extend_from_slice(entry.name.as_bytes());
    out.extend_from_slice(&(entry.secret.len() as u16).to_le_bytes());
    out.extend_from_slice(&entry.secret);
    out
}

fn decode_record(plain: &[u8]) -> Result<AppEntry, FileError> {
    let mut rest = plain;
    let head = take(&mut rest, 5)?;
    let uid = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let app = core::str::from_utf8(take(&mut rest, head[4] as usize)?)
        .map_err(|_| FileError::BadRecord)?;
    let name_len = take(&mut rest, 1)?[0] as usize;
    let name =
        core::str::from_utf8(take(&mut rest, name_len)?).map_err(|_| FileError::BadRecord)?;
    let secret_len = take(&mut rest, 2)?;
    let secret_len = u16::from_le_bytes([secret_len[0], secret_len[1]]) as usize;
    let secret = take(&mut rest, secret_len)?;
    let app_ok = crate::app_of_label(&alloc::format!("app:{app}")) == Some(app);
    if !app_ok || !valid_app_name(name) || !valid_app_secret(secret) || !rest.is_empty() {
        return Err(FileError::BadRecord);
    }
    Ok(AppEntry {
        uid,
        app: String::from(app),
        name: String::from(name),
        secret: secret.to_vec(),
    })
}
