//! The userspace drivers embedded in the disk image: the virtio-sound stack
//! (`LAZYOS_SOUND=1`, docs/driver-plan.md D6) and the virtio-net stack
//! (`LAZYOS_NET=1`, docs/networking-plan.md N1).
//!
//! Without a supervisor the kernel boots a driver directly; with
//! `LAZYOS_SERVICES=1` `init` starts it from its manifest instead. Each goes to
//! its `fhs::bin` path in `/system/bin`.

use std::ffi::OsStr;
use std::path::PathBuf;

use crate::os_image::Sink;

fn enabled(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Add the ELF built for the `user` binary `bin` to the image at `path`.
fn add(sink: &mut dyn Sink, path: &str, bin: &str) {
    let variable = format!("CARGO_BIN_FILE_USER_{bin}");
    let artifact =
        std::env::var_os(&variable).unwrap_or_else(|| panic!("user {bin} artifact not found"));
    sink.add_file(path, PathBuf::from(artifact));
}

/// The `netfix` fixture: `LAZYOS_NETFIX`, or the one `tools/abi/build.py` left
/// in `target/abi/fixtures`.
fn netfix() -> Option<PathBuf> {
    fixture("LAZYOS_NETFIX", "netfix")
}

/// A Linux fixture: the file `variable` names, or the one `tools/abi/build.py`
/// left in `target/abi/fixtures/<name>.elf`.
fn fixture(variable: &str, name: &str) -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed={variable}");
    if let Some(path) = std::env::var_os(variable).map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let path = root.join(format!("target/abi/fixtures/{name}.elf"));
    path.is_file().then_some(path)
}

/// Embed the drivers this build asked for. The desktop profile always ships the
/// sound stack, so its shell has `beep`.
pub fn embed(sink: &mut dyn Sink, desktop: bool) {
    let sound = desktop || enabled("LAZYOS_SOUND");
    let usb = enabled("LAZYOS_USB");
    let netd = enabled("LAZYOS_NETD");
    let net = netd || enabled("LAZYOS_NET");
    // `devctl` (issue #481) shows the devices, who owns them and the class
    // rules that confine each driver: wherever there is a driver to look at.
    if sound || usb || net {
        add(sink, fhs::bin::DEVCTL, "devctl");
    }
    if sound {
        add(sink, fhs::bin::SNDD, "sndd");
        // `audiod`, the system mixer every audio client talks to
        // (docs/audio-plan.md), and `mixer`, its volume control command.
        add(sink, fhs::bin::AUDIOD, "audiod");
        add(sink, fhs::bin::MIXER, "mixer");
        // `beep`, the smallest audio client: `audiod` spawns it under `demo=1`.
        add(sink, fhs::bin::BEEP, "beep");
        // `modplay`, the tracker-module player (docs/tracker-plan.md).
        add(sink, fhs::bin::MODPLAY, "modplay");
    }
    // The USB HID driver (docs/usb-hid-plan.md U2).
    if usb {
        add(sink, fhs::bin::USBD, "usbd");
    }
    // `LAZYOS_NETD=1` adds the stack service and its tools, and needs the driver.
    if net {
        add(sink, fhs::bin::NETDRV, "netdrv");
        // `nicctl` prints the card and carries the evidence clients the driver
        // spawns under `demo=1`.
        add(sink, fhs::bin::NICCTL, "nicctl");
    }
    if netd {
        add(sink, fhs::bin::NETD, "netd");
        // `netctl` and `ping`, the stack's shell commands and evidence clients.
        add(sink, fhs::bin::NETCTL, "netctl");
        add(sink, fhs::bin::PING, "ping");
        // `nc` and `nslookup`: sockets and name lookups (stage N3).
        add(sink, fhs::bin::NC, "nc");
        add(sink, fhs::bin::NSLOOKUP, "nslookup");
        // `ftp`, the passive-mode client (stage N4).
        add(sink, fhs::bin::FTP, "ftp");
        // `ftpfuse`, the same server mounted as a directory under `/mnt`
        // (docs/smb-plan.md: a network filesystem on the FUSE mechanism).
        add(sink, fhs::bin::FTPFUSE, "ftpfuse");
        // `netfix`, the `std::net` Linux fixture the `AF_INET` shim is judged
        // by (stage N5), when the harness built one (`tools/abi/build.py`);
        // without a musl toolchain the image simply lacks it.
        if let Some(path) = netfix() {
            println!("cargo:rerun-if-changed={}", path.display());
            sink.add_file(fhs::bin::NETFIX, path);
        }
        // Bulk TCP throughput (docs/performance-plan.md P4,
        // `tools/net/bulk.py`): the native client always, its Linux twin when
        // the fixtures were built.
        add(sink, fhs::bin::NETBULK, "netbulk");
        if let Some(path) = fixture("LAZYOS_NETBULK", "netbulk") {
            println!("cargo:rerun-if-changed={}", path.display());
            sink.add_file(fhs::bin::NETBULK_LINUX, path);
        }
    }
}
