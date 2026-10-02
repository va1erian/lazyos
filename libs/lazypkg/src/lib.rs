//! LazyOS application package (`.lzp`) reader.
//!
//! A package is a zip archive holding a [`Manifest`], one or more binaries, the
//! three shell icons, and optional interfaces, documentation and resources. The
//! caller hands the reader the whole archive as `&[u8]`; nothing here touches
//! the OS, the filesystem or Messenger. The authoritative description of the
//! format is `docs/packages.md`, and `tools/pkg/build.py` builds archives that
//! this reader accepts (both enforce the same layout rules and both are tested).
//!
//! **The archive is untrusted.** Every length, count and offset is checked
//! against the archive size and the [`MAX_ENTRIES`], [`MAX_ENTRY_UNCOMPRESSED`],
//! [`MAX_TOTAL_UNCOMPRESSED`], [`MAX_NAME_LEN`] and [`MAX_MANIFEST`] caps before
//! it is used or allocated, so a zip bomb or a claimed 4 GiB entry is refused
//! cheaply. [`Package::open`] returns either a fully validated package or an
//! error; no partially filled state is ever exposed. There is no `unsafe` in
//! this crate.
//!
//! ```no_run
//! # fn main() -> Result<(), lazypkg::OpenError> {
//! let archive: &[u8] = &[];
//! let package = lazypkg::Package::open(archive)?;
//! let icon = package.read("icons/app-16.png").expect("validated at open");
//! println!("{}", package.install_dir());
//! # Ok(())
//! # }
//! ```

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

extern crate alloc;

mod error;
mod files;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod grammar;
mod inflate;
mod layout;
mod manifest;
mod path;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod testzip;
mod validate;
mod version;
mod zip;

pub use error::{ManifestError, OpenError, Problem, ReadError};
pub use files::HOME_VAR;
pub use manifest::{App, Category, Entry, Manifest, MimeHandler, Permissions};
pub use version::{Version, VersionError, MAX_VERSION_LEN};
pub use zip::EntryInfo;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use zip::ZipEntry;

/// Most entries a package may hold.
pub const MAX_ENTRIES: usize = 1024;
/// Most bytes all entries may expand to together (64 MiB).
pub const MAX_TOTAL_UNCOMPRESSED: u64 = 64 * 1024 * 1024;
/// Most bytes one entry may expand to (16 MiB).
pub const MAX_ENTRY_UNCOMPRESSED: u32 = 16 * 1024 * 1024;
/// Longest entry name, in bytes.
pub const MAX_NAME_LEN: usize = 255;
/// Largest `manifest.toml`, in bytes (1 MiB).
pub const MAX_MANIFEST: usize = 1024 * 1024;

/// The 8-byte PNG signature every icon must start with.
const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// A validated package: the archive, its entries, the manifest and the digest.
#[derive(Debug)]
pub struct Package<'a> {
    bytes: &'a [u8],
    entries: Vec<ZipEntry<'a>>,
    manifest: Manifest,
    digest: [u8; 32],
}

impl<'a> Package<'a> {
    /// Parse the central directory, validate structure and paths, parse and
    /// validate the manifest.
    ///
    /// The error for manifest problems carries every problem found
    /// ([`ManifestError`]); structural errors are reported before the manifest
    /// is read.
    pub fn open(bytes: &'a [u8]) -> Result<Package<'a>, OpenError> {
        let entries = zip::parse(bytes)?;
        let files = layout::validate(&entries)?;
        let manifest_entry = entries
            .iter()
            .find(|entry| entry.info.name == layout::MANIFEST)
            .ok_or(OpenError::NoManifest)?;
        if manifest_entry.info.size as usize > MAX_MANIFEST {
            return Err(OpenError::Layout {
                name: String::from(layout::MANIFEST),
                reason: "is larger than 1 MiB",
            });
        }
        let manifest_bytes = extract(bytes, manifest_entry).map_err(manifest_read_error)?;
        let text = core::str::from_utf8(&manifest_bytes)
            .map_err(|_| manifest_problem(String::from("manifest.toml is not valid UTF-8")))?;
        let manifest = manifest::parse(text)?;
        let problems = validate::validate(&manifest, &files);
        if !problems.is_empty() {
            return Err(OpenError::Manifest(ManifestError::new(problems)));
        }
        for icon in layout::REQUIRED_ICONS {
            let entry = entries
                .iter()
                .find(|entry| entry.info.name == icon)
                .ok_or_else(|| OpenError::BadIcon { name: icon.into() })?;
            let data =
                extract(bytes, entry).map_err(|_| OpenError::BadIcon { name: icon.into() })?;
            if !data.starts_with(&PNG_SIG) {
                return Err(OpenError::BadIcon { name: icon.into() });
            }
        }
        let digest = lazyos_crypto::sha256::sha256(bytes);
        Ok(Package {
            bytes,
            entries,
            manifest,
            digest,
        })
    }

    /// The validated manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Every entry in central-directory order.
    pub fn entries(&self) -> impl Iterator<Item = EntryInfo<'a>> + '_ {
        self.entries.iter().map(|entry| entry.info)
    }

    /// Inflate (or copy, for stored) one entry into a new `Vec`, verifying size
    /// and CRC-32.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, ReadError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.info.name == name)
            .ok_or(ReadError::NoSuchEntry)?;
        if entry.info.is_dir {
            return Err(ReadError::IsDirectory);
        }
        extract(self.bytes, entry)
    }

    /// SHA-256 of the whole archive.
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// `<system_name>/<version>-<first 8 lowercase hex chars of digest>`: the
    /// install directory relative to `/data/apps`.
    pub fn install_dir(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut suffix = String::with_capacity(8);
        for byte in &self.digest[..4] {
            suffix.push(HEX[(byte >> 4) as usize] as char);
            suffix.push(HEX[(byte & 0x0f) as usize] as char);
        }
        format!(
            "{}/{}-{}",
            self.manifest.app.system_name, self.manifest.app.version, suffix
        )
    }
}

/// Parse and validate a `manifest.toml` read back from an install directory
/// (or anywhere else outside an archive). Everything the manifest says about
/// itself is checked; whether `entry.binary` and the icons exist is not, because
/// there is no archive to look in. The installer uses this to rebuild an app's
/// policy and MIME registrations at boot from the manifest it stored.
pub fn parse_manifest(text: &str) -> Result<Manifest, ManifestError> {
    let manifest = manifest::parse(text)?;
    let problems = validate::validate_standalone(&manifest);
    if problems.is_empty() {
        Ok(manifest)
    } else {
        Err(ManifestError::new(problems))
    }
}

/// Read and CRC-check one entry. Bounds were validated at open; the checked
/// arithmetic here is a belt-and-braces guard that must never fire.
fn extract(bytes: &[u8], entry: &ZipEntry<'_>) -> Result<Vec<u8>, ReadError> {
    let name = entry.info.name;
    let end = entry
        .data_start
        .checked_add(entry.info.compressed_size as usize)
        .ok_or_else(|| ReadError::Corrupt { name: name.into() })?;
    let data = bytes
        .get(entry.data_start..end)
        .ok_or_else(|| ReadError::Corrupt { name: name.into() })?;
    let out =
        inflate::decompress(entry.method, data, entry.info.size).map_err(|error| match error {
            inflate::InflateError::Corrupt => ReadError::Corrupt { name: name.into() },
            inflate::InflateError::SizeMismatch { expected, actual } => ReadError::SizeMismatch {
                name: name.into(),
                expected,
                actual,
            },
        })?;
    let actual = inflate::crc32(&out);
    if actual != entry.info.crc32 {
        return Err(ReadError::CrcMismatch {
            name: name.into(),
            expected: entry.info.crc32,
            actual,
        });
    }
    Ok(out)
}

fn manifest_read_error(error: ReadError) -> OpenError {
    manifest_problem(format!("manifest.toml could not be read: {error}"))
}

fn manifest_problem(message: String) -> OpenError {
    OpenError::Manifest(ManifestError::new(alloc::vec![Problem::new(message)]))
}
