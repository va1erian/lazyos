//! The control tier's rules (docs/dbgd-plan.md, v2): which services a
//! client may restart or hot-reload, how an uploaded binary is assembled
//! and checked, and the trial window `init` holds a reloaded service to.
//!
//! A hot reload goes: `service.upload` chunks into `dbgd`'s staging file
//! ([`staged_path`]), tracked by an [`Upload`]; `service.reload` hands the
//! finished file and its SHA-256 to `init`, which copies it somewhere only
//! root can write, checks the digest again and restarts the service from
//! it. If the new binary is not still running when the trial ends, `init`
//! puts the image's binary back. Both `dbgd` and `init` check
//! [`reloadable`], so neither alone decides what may be replaced.
//!
//! An app is swapped the same way with a package: `app.upload` stages an
//! `.lzp` under the reserved name [`PACKAGE`], `app.install` hands it to
//! `pkgd` with its SHA-256 and has `init` relaunch the running instances
//! ([`valid_app_id`]). There is no trial: `pkgd` validates the package and
//! a bad app is the developer's to see and replace.

use alloc::string::String;
use alloc::vec::Vec;

/// Longest service name.
pub const MAX_NAME: usize = 32;
/// Largest service binary accepted (bytes).
pub const MAX_BINARY: u64 = 32 << 20;
/// Largest package accepted (bytes): a game with its data fits.
pub const MAX_PACKAGE: u64 = 256 << 20;
/// The upload name of a package (never a service's: [`reloadable`] refuses
/// it), staged as [`staged_package_path`].
pub const PACKAGE: &str = "package";
/// Longest app id (`system_name`, or a built-in app's id).
pub const MAX_APP_ID: usize = 64;
/// Bounds and default of the trial window (milliseconds).
pub const TRIAL_MIN_MS: u64 = 1_000;
pub const TRIAL_MAX_MS: u64 = 120_000;
pub const TRIAL_DEFAULT_MS: u64 = 10_000;
/// What `control.begin` must be given: a client opens control on purpose,
/// never by replaying a read-only script.
pub const CONFIRM: &str = "control";

/// Services no client may restart or replace: `messengerd` (it claims the
/// kernel's bootstrap channel once per boot and could not listen again) and
/// `dbgd` itself (the connection that would have to watch the reload).
pub const NEVER: &[&str] = &["messengerd", "dbgd"];

/// Why a control request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    BadName,
    Never,
    BadDigest,
    BadData,
    OutOfOrder,
    TooBig,
    Incomplete,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Refusal::BadName => "a service name is 1..32 of a-z, 0-9, '-' and '_'",
            Refusal::Never => "messengerd and dbgd cannot be restarted or replaced remotely",
            Refusal::BadDigest => "sha256 is 64 hex characters",
            Refusal::BadData => "data is not base64",
            Refusal::OutOfOrder => "chunks go in order: offset must be the bytes received so far",
            Refusal::TooBig => "the binary is larger than total or the limit",
            Refusal::Incomplete => "the upload is not finished (or was never started)",
        }
    }
}

/// Whether `name` is a well-formed service name (so it is also a safe file
/// name: no `/`, no `.`).
pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Whether a client may restart or replace the service `name`.
pub fn reloadable(name: &str) -> Result<(), Refusal> {
    if !valid_name(name) {
        return Err(Refusal::BadName);
    }
    if NEVER.contains(&name) || name == PACKAGE {
        return Err(Refusal::Never);
    }
    Ok(())
}

/// Whether `id` is a well-formed app id: a package `system_name`
/// (`org.lazy.doom`) or a built-in app's id; lower case, digits, `.`, `-`,
/// `_`, starting with a letter or digit.
pub fn valid_app_id(id: &str) -> bool {
    (1..=MAX_APP_ID).contains(&id.len())
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && !id.contains("..")
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// Where `dbgd` stages an uploaded package; `pkgd` installs nothing else on
/// `dbgd`'s word.
pub fn staged_package_path() -> String {
    alloc::format!("{}/{PACKAGE}.lzp", fhs::state::DBGD_STAGE)
}

/// Where `dbgd` stages the upload for `name` (a [`valid_name`], or
/// [`PACKAGE`] for [`staged_package_path`]).
pub fn staged_path(name: &str) -> String {
    if name == PACKAGE {
        return staged_package_path();
    }
    alloc::format!("{}/{name}.elf", fhs::state::DBGD_STAGE)
}

/// Where `init` keeps the binary it runs for `name` (a [`valid_name`]).
pub fn reload_path(name: &str) -> String {
    alloc::format!("{}/{name}", fhs::state::INIT_RELOAD)
}

/// Whether `path` is exactly the staging file of `name`: `init` reads
/// nothing else on `dbgd`'s word.
pub fn is_staged_path(name: &str, path: &str) -> bool {
    valid_name(name) && path == staged_path(name)
}

/// A SHA-256 digest given in hex.
pub fn parse_digest(text: &str) -> Result<[u8; 32], Refusal> {
    let bytes = crate::auth::unhex(text).ok_or(Refusal::BadDigest)?;
    bytes.try_into().map_err(|_| Refusal::BadDigest)
}

/// Decode standard base64 (`A-Z a-z 0-9 + /`, `=` padding required to a
/// multiple of four). Anything else is refused, never skipped.
pub fn base64_decode(text: &str) -> Result<Vec<u8>, Refusal> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(Refusal::BadData);
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let quads = bytes.len() / 4;
    for (index, quad) in bytes.chunks(4).enumerate() {
        let last = index + 1 == quads;
        let pad = quad.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err(Refusal::BadData);
        }
        let mut word = 0u32;
        for (at, byte) in quad.iter().enumerate() {
            let six = if at >= 4 - pad {
                0
            } else {
                sextet(*byte).ok_or(Refusal::BadData)?
            };
            word = word << 6 | six;
        }
        let [_, a, b, c] = word.to_be_bytes();
        out.extend_from_slice(&[a, b, c][..3 - pad]);
    }
    Ok(out)
}

fn sextet(byte: u8) -> Option<u32> {
    Some(match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

/// One service binary being uploaded: chunks arrive in order, the total is
/// fixed by the first one, and nothing past it is accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub name: String,
    pub total: u64,
    pub received: u64,
}

/// What to do with a chunk's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    /// Start the file afresh (offset 0).
    Create,
    /// Append to it.
    Append,
}

impl Upload {
    /// Account for a chunk of `len` bytes at `offset` of a `total`-byte
    /// binary for `name`. Offset 0 always starts a new upload (a client
    /// that lost its connection starts over); any other offset must
    /// continue the current upload of the same name and total.
    pub fn accept(
        current: &mut Option<Upload>,
        name: &str,
        offset: u64,
        total: u64,
        len: u64,
    ) -> Result<Write, Refusal> {
        let limit = if name == PACKAGE {
            MAX_PACKAGE
        } else {
            reloadable(name)?;
            MAX_BINARY
        };
        if total == 0 || total > limit {
            return Err(Refusal::TooBig);
        }
        let write = if offset == 0 {
            *current = Some(Upload {
                name: String::from(name),
                total,
                received: 0,
            });
            Write::Create
        } else {
            match current {
                Some(up) if up.name == name && up.total == total && up.received == offset => {
                    Write::Append
                }
                _ => return Err(Refusal::OutOfOrder),
            }
        };
        let Some(up) = current.as_mut() else {
            return Err(Refusal::OutOfOrder);
        };
        if up.received + len > up.total {
            *current = None;
            return Err(Refusal::TooBig);
        }
        up.received += len;
        Ok(write)
    }

    /// The finished upload of `name`, if there is one.
    pub fn finished<'a>(current: &'a Option<Upload>, name: &str) -> Result<&'a Upload, Refusal> {
        match current {
            Some(up) if up.name == name && up.received == up.total => Ok(up),
            _ => Err(Refusal::Incomplete),
        }
    }
}
