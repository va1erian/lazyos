//! `LAZYOS_NETBOOT=1`: also write `target/netboot/`, the stick's payload as
//! loose files plus a manifest (docs/netboot-plan.md N1, docs/usb-stick.md):
//!
//! ```text
//! kernel-x86_64   the trimmed kernel
//! ramdisk         usb_ramdisk::write: lazyos.cfg + the ext2 OS volume
//! bootx64.efi     the bootloader's UEFI stage
//! boot.json       the loader's config
//! manifest.json   id, revision, dirty, built_at, host, switches, files (size, sha256)
//! manifest.sig    only with LAZYOS_NETBOOT_KEY=<key.sec>: Ed25519 over manifest.json
//! ```
//!
//! The four payload files are the ones `usb_image` puts in the stick's FAT
//! partition, from the same [`crate::usb_payload::Payload`]: the ramdisk is
//! written once and copied, so a build with `LAZYOS_USB_IMAGE=1` too writes
//! identical bytes to both places. The stick's profile rule applies
//! (`usb_stick::check_profile`). The pure parts (id, manifest, signature) are
//! in `netboot_manifest.rs`.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use crate::usb_ramdisk::Written;
use manifest::{BuildInfo, FileEntry};

#[path = "netboot_manifest.rs"]
pub mod manifest;

/// Where the payload goes, under the manifest directory.
pub const OUTPUT_DIR: &str = "target/netboot";

/// The files the manifest lists, in its order.
pub const KERNEL: &str = "kernel-x86_64";
pub const RAMDISK: &str = "ramdisk";
pub const LOADER: &str = "bootx64.efi";
pub const BOOT_JSON: &str = "boot.json";
const MANIFEST: &str = "manifest.json";
const SIGNATURE: &str = "manifest.sig";

/// Whether this build writes the payload.
pub fn enabled() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_NETBOOT");
    println!("cargo:rerun-if-env-changed=LAZYOS_NETBOOT_KEY");
    std::env::var_os("LAZYOS_NETBOOT").as_deref() == Some(OsStr::new("1"))
}

/// The `lazyos.cfg` line that names the build (nothing without `LAZYOS_NETBOOT=1`).
pub fn image_id_line(info: Option<&BuildInfo>) -> String {
    info.map_or_else(String::new, |info| format!("image.id={}\n", info.id))
}

/// Run `git` in `dir`; its trimmed standard output when it succeeds.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// This machine's name, for the manifest.
fn host_name() -> String {
    let command = Command::new("hostname")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
    command
        .filter(|name| !name.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Identify this build: the time, the revision the kernel embeds, whether the
/// tree is dirty, this host and the `LAZYOS_*` switches set now.
pub fn detect(source: &Path) -> BuildInfo {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let env = std::env::var("LAZYOS_REVISION").ok();
    let head = git(source, &["rev-parse", "HEAD"]);
    let (revision, problem) = manifest::revision(env.as_deref(), head.as_deref());
    if let Some(problem) = problem {
        println!("cargo:warning=netboot: {problem}");
    }
    let dirty = git(source, &["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    let vars = std::env::vars_os().filter_map(|(name, value)| {
        Some((
            name.into_string().ok()?,
            value.into_string().unwrap_or_default(),
        ))
    });
    let info = BuildInfo::new(now, &revision, dirty, &host_name(), vars);
    assert!(
        manifest::valid_id(&info.id),
        "internal: build id {:?} is not a valid image id",
        info.id
    );
    info
}

/// The signing key named by `LAZYOS_NETBOOT_KEY=<path to key.sec>`, if set.
pub fn key_from_env() -> Result<Option<SigningKey>, String> {
    let Some(path) = std::env::var_os("LAZYOS_NETBOOT_KEY").filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    crate::os_image::rerun_if_changed(&path);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("LAZYOS_NETBOOT_KEY {}: {e}", path.display()))?;
    manifest::parse_key(&text)
        .map(Some)
        .map_err(|e| format!("LAZYOS_NETBOOT_KEY {}: {e}", path.display()))
}

/// The payload to write: the files `usb_image` also puts on the stick.
pub struct Inputs<'a> {
    pub kernel: &'a Path,
    pub ramdisk: &'a Path,
    pub loader: &'a [u8],
    pub boot_json: &'a [u8],
}

/// What [`write`] produced.
#[derive(Debug)]
pub struct Output {
    /// The exact bytes of `manifest.json`.
    pub manifest: String,
    /// Whether `manifest.sig` was written.
    pub signed: bool,
}

/// Put `bytes` at `dir/name` through a temporary file.
fn put(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let temp = dir.join(format!("{name}.tmp"));
    std::fs::write(&temp, bytes).map_err(|e| format!("{}: {e}", temp.display()))?;
    replace(&temp, &dir.join(name))
}

fn replace(temp: &Path, target: &Path) -> Result<(), String> {
    std::fs::rename(temp, target).map_err(|e| {
        format!(
            "cannot replace {} ({e}); is QEMU or a pusher using it?",
            target.display()
        )
    })
}

/// Copy `source` to `dir/name` through a temporary file, hashing as it goes.
fn copy_hashed(source: &Path, dir: &Path, name: &str) -> Result<FileEntry, String> {
    let mut input = File::open(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let temp = dir.join(format!("{name}.tmp"));
    let mut output = File::create(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|e| format!("{}: {e}", source.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .map_err(|e| format!("{}: {e}", temp.display()))?;
        size += read as u64;
    }
    output
        .sync_all()
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    drop(output);
    replace(&temp, &dir.join(name))?;
    Ok(FileEntry {
        name: name.into(),
        size,
        sha256: manifest::hex(&hasher.finalize()),
    })
}

fn entry_of(name: &str, bytes: &[u8]) -> FileEntry {
    FileEntry {
        name: name.into(),
        size: bytes.len() as u64,
        sha256: manifest::hex(&Sha256::digest(bytes)),
    }
}

/// Write the payload, `manifest.json` and, with `key`, `manifest.sig` into
/// `dir`. Without a key a `manifest.sig` left by an earlier build is removed:
/// it would not match the new manifest.
pub fn write(
    dir: &Path,
    inputs: &Inputs,
    info: &BuildInfo,
    key: Option<&SigningKey>,
) -> Result<Output, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // The manifest and its signature go last, so a half-written directory
    // never carries a signature for other files.
    for stale in [MANIFEST, SIGNATURE] {
        let _ = std::fs::remove_file(dir.join(stale));
    }
    let files = vec![
        copy_hashed(inputs.kernel, dir, KERNEL)?,
        copy_hashed(inputs.ramdisk, dir, RAMDISK)?,
        {
            put(dir, LOADER, inputs.loader)?;
            entry_of(LOADER, inputs.loader)
        },
        {
            put(dir, BOOT_JSON, inputs.boot_json)?;
            entry_of(BOOT_JSON, inputs.boot_json)
        },
    ];
    let text = manifest::manifest_json(info, &files);
    put(dir, MANIFEST, text.as_bytes())?;
    if let Some(key) = key {
        put(
            dir,
            SIGNATURE,
            manifest::signature_file(key, text.as_bytes()).as_bytes(),
        )?;
    }
    Ok(Output {
        manifest: text,
        signed: key.is_some(),
    })
}

/// Write `target/netboot/` for the build `info`, signed when
/// `LAZYOS_NETBOOT_KEY` names a key, and print the summary warnings.
pub fn build(
    source: &Path,
    inputs: &Inputs,
    written: &Written,
    info: &BuildInfo,
) -> Result<(), String> {
    let key = key_from_env()?;
    let dir = source.join(OUTPUT_DIR);
    let output = write(&dir, inputs, info, key.as_ref())?;
    println!(
        "cargo:warning=netboot payload {} in {} (manifest.json {} bytes{})",
        info.id,
        dir.display(),
        output.manifest.len(),
        if output.signed { ", signed" } else { "" }
    );
    if !output.signed {
        println!(
            "cargo:warning=netboot payload is unsigned (LAZYOS_NETBOOT_KEY is not set): \
             manifest.sig is missing, so the registry refuses it and it cannot be pushed"
        );
    }
    println!("cargo:warning={}", breakdown(written));
    Ok(())
}

/// The one-line `cargo:warning=` account of what the ramdisk is made of, for
/// the 308 MiB question of docs/netboot-plan.md section 6.
pub fn breakdown(written: &Written) -> String {
    let mib = |bytes: u64| bytes >> 20;
    let sized = written.files_bytes + written.packages_bytes + written.free_bytes;
    let regrown = written.os_bytes.saturating_sub(sized);
    format!(
        "netboot ramdisk {} MiB = files {} MiB + core packages unpacked at first boot {} MiB \
         + free space {} MiB (LAZYOS_USB_ROOT_FREE) + regrown {} MiB + MBR and /boot {} MiB",
        mib(written.bytes),
        mib(written.files_bytes),
        mib(written.packages_bytes),
        mib(written.free_bytes),
        mib(regrown),
        mib(written.bytes - written.os_bytes),
    )
}
