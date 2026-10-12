//! Wi-Fi in the disk image (`LAZYOS_WIFI=1`; docs/wifi-rtw88-plan.md R0,
//! docs/wifi-prerequisites-plan.md 3.8 and 3.10).
//!
//! The switch puts the Wi-Fi stack in the image: the RTL8822CE driver
//! `wifid` (which `devd` starts as `wlan0` for a `10ec:c822` function,
//! `libs/devmatch`, under `init`'s `wifid` driver row) and the firmware it
//! loads, each file of `tools/firmware/manifest.txt` marked `wifi` placed at
//! `fhs::FIRMWARE/<name>` with its licence beside it. The station stack's own
//! programs join here as they land.
//!
//! **Firmware.** `tools/firmware/fetch.py` downloads the pinned files into
//! `target/firmware/<name>`; this module checks each again against the
//! manifest (size and SHA-256) and adds it to the OS file list, so
//! `/system/.image-manifest` tracks it and an in-place update replaces it.
//! A missing or altered file fails the build with the command to run: an
//! image that asked for Wi-Fi never comes without its firmware. The bytes
//! are never compiled into a program (the blobs' licences are their own).
//!
//! **Requirements.** Refused without `LAZYOS_NETD=1` (a wireless card is a
//! network interface of `netd`'s) and with `LAZYOS_DEVD=0` (only `devd`
//! starts `wifid`, for the card it found). The front ends set what is needed
//! (`python tools/run_demo.py --wifi`, the launcher's Wi-Fi checkboxes).
//!
//! With the switch unset nothing changes.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::os_image::Sink;

/// The pinned firmware list, relative to the repository root.
pub const MANIFEST: &str = "tools/firmware/manifest.txt";
/// Where `tools/firmware/fetch.py` leaves the files.
pub const CACHE: &str = "target/firmware";
/// The manifest's switch name for `LAZYOS_WIFI=1`.
pub const SWITCH: &str = "wifi";

/// One pinned firmware file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// Vendor-relative (`rtw88/rtw8822c_fw.bin`).
    pub name: String,
    pub switch: String,
    pub sha256: [u8; 32],
    pub size: u64,
    /// The file holding its licence text (listed too).
    pub licence: String,
}

fn on(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Whether a build with `wifi` can ship it.
pub fn requirements(wifi: bool, netd: bool, devd_off: bool) -> Result<(), String> {
    if wifi && !netd {
        return Err(
            "LAZYOS_WIFI=1 needs LAZYOS_NETD=1 (a wireless card is one of netd's \
                    interfaces); `python tools/run_demo.py --wifi` sets it"
                .into(),
        );
    }
    if wifi && devd_off {
        return Err(
            "LAZYOS_WIFI=1 needs devd (drop LAZYOS_DEVD=0): only devd starts wifid, \
                    for the card it found"
                .into(),
        );
    }
    Ok(())
}

fn hex_digest(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Parse the manifest (`name | switch | sha256 | bytes | licence | url`),
/// with `libs/fwload`'s name rules; every licence must be listed.
pub fn parse_manifest(text: &str) -> Result<Vec<Pin>, String> {
    let mut pins: Vec<Pin> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |why: String| format!("line {}: {why}", index + 1);
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        let [name, switch, digest, size, licence, url] = fields[..] else {
            return Err(at(format!("expected 6 fields, got {}", fields.len())));
        };
        for file in [name, licence] {
            fwload::check_name(file).map_err(|why| at(format!("`{file}`: {why}")))?;
        }
        let sha256 =
            hex_digest(digest).ok_or_else(|| at("the digest is not 64 hex digits".into()))?;
        let size: u64 = size
            .parse()
            .ok()
            .filter(|&size| size > 0 && size <= fwload::DEFAULT_MAX_BYTES as u64)
            .ok_or_else(|| at(format!("bad size `{size}`")))?;
        if !url.starts_with("https://") {
            return Err(at("the URL is not https".into()));
        }
        if pins.iter().any(|pin| pin.name == name) {
            return Err(at(format!("{name} is listed twice")));
        }
        pins.push(Pin {
            name: name.into(),
            switch: switch.into(),
            sha256,
            size,
            licence: licence.into(),
        });
    }
    if let Some(pin) = pins
        .iter()
        .find(|pin| !pins.iter().any(|other| other.name == pin.licence))
    {
        return Err(format!(
            "{}: its licence {} is not listed",
            pin.name, pin.licence
        ));
    }
    Ok(pins)
}

/// The pins `switch` installs, with the licences they name.
pub fn selected<'a>(pins: &'a [Pin], switch: &str) -> Vec<&'a Pin> {
    let mut chosen: Vec<&Pin> = pins.iter().filter(|pin| pin.switch == switch).collect();
    for pin in pins {
        let needed = chosen.iter().any(|wanted| wanted.licence == pin.name);
        if needed && !chosen.iter().any(|have| have.name == pin.name) {
            chosen.push(pin);
        }
    }
    chosen
}

/// Whether `bytes` are exactly what `pin` names.
pub fn check_bytes(pin: &Pin, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 != pin.size {
        return Err(format!(
            "{}: {} bytes, pinned {}",
            pin.name,
            bytes.len(),
            pin.size
        ));
    }
    if lazyos_crypto::sha256::sha256(bytes) != pin.sha256 {
        return Err(format!("{}: SHA-256 does not match the pin", pin.name));
    }
    Ok(())
}

/// `(destination, source)` for every file of `switch`, each checked in the
/// cache `cache`.
pub fn firmware_files(
    pins: &[Pin],
    switch: &str,
    cache: &Path,
) -> Result<Vec<(String, PathBuf)>, String> {
    let mut files = Vec::new();
    for pin in selected(pins, switch) {
        let source = cache.join(&pin.name);
        let bytes = std::fs::read(&source).map_err(|error| {
            format!(
                "{} is missing ({error}); run `python tools/firmware/fetch.py`",
                source.display()
            )
        })?;
        check_bytes(pin, &bytes).map_err(|why| {
            format!(
                "{why} ({}); run `python tools/firmware/fetch.py`",
                source.display()
            )
        })?;
        files.push((format!("{}/{}", fhs::FIRMWARE, pin.name), source));
    }
    Ok(files)
}

/// Add the ELF built for the `user` binary `bin` at `path`.
fn add_program(sink: &mut dyn Sink, path: &str, bin: &str) {
    let variable = format!("CARGO_BIN_FILE_USER_{bin}");
    let artifact =
        std::env::var_os(&variable).unwrap_or_else(|| panic!("user {bin} artifact not found"));
    sink.add_file(path, PathBuf::from(artifact));
}

/// Whether the image ships Wi-Fi (`LAZYOS_WIFI=1`); panics on a build that
/// cannot ([`requirements`]).
pub fn enabled() -> bool {
    let wifi = on("LAZYOS_WIFI");
    println!("cargo:rerun-if-env-changed=LAZYOS_DEVD");
    let devd_off = std::env::var_os("LAZYOS_DEVD").as_deref() == Some(OsStr::new("0"));
    if let Err(reason) = requirements(wifi, on("LAZYOS_NETD"), devd_off) {
        panic!("{reason}");
    }
    wifi
}

/// Add `wifid` and the Wi-Fi firmware when [`enabled`].
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/wifi_embed.rs");
    let manifest = manifest_dir.join(MANIFEST);
    let cache = manifest_dir.join(CACHE);
    // Watched even when unused: a fetch or a bump reruns the build.
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", cache.display());
    if !enabled() {
        return;
    }
    add_program(sink, fhs::bin::WIFID, "wifid");
    // The station manager and its command line (docs/architecture/wifi.md).
    add_program(sink, fhs::bin::WLANMD, "wlanmd");
    add_program(sink, fhs::bin::WIFICTL, "wifictl");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()));
    let pins = parse_manifest(&text).unwrap_or_else(|why| panic!("{}: {why}", manifest.display()));
    let files =
        firmware_files(&pins, SWITCH, &cache).unwrap_or_else(|why| panic!("LAZYOS_WIFI=1: {why}"));
    for (dest, source) in files {
        println!("cargo:rerun-if-changed={}", source.display());
        sink.add_file(&dest, source);
    }
}
