//! The userspace drivers embedded in the disk image: the virtio-sound stack
//! (`LAZYOS_SOUND=1`, docs/driver-plan.md D6) and the virtio-net stack
//! (`LAZYOS_NET=1`, docs/networking-plan.md N1).
//!
//! Without a supervisor the kernel boots a driver directly; with
//! `LAZYOS_SERVICES=1` `init` starts it from its manifest instead. The 8.3-safe
//! on-disk names are what the kernel's FAT reader resolves.

use std::ffi::OsStr;
use std::path::PathBuf;

fn enabled(variable: &str) -> bool {
    println!("cargo:rerun-if-env-changed={variable}");
    std::env::var_os(variable).as_deref() == Some(OsStr::new("1"))
}

/// Add the ELF built for the `user` binary `bin` to the image as `name`.
fn add(builder: &mut bootloader::DiskImageBuilder, name: &str, bin: &str) {
    let variable = format!("CARGO_BIN_FILE_USER_{bin}");
    let path =
        std::env::var_os(&variable).unwrap_or_else(|| panic!("user {bin} artifact not found"));
    builder.set_file(String::from(name), PathBuf::from(path));
}

/// The `netfix` fixture: `LAZYOS_NETFIX`, or the one `tools/abi/build.py` left
/// in `target/abi/fixtures`.
fn netfix() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LAZYOS_NETFIX").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let path = root.join("target/abi/fixtures/netfix.elf");
    path.is_file().then_some(path)
}

/// Embed the drivers this build asked for. The desktop profile always ships the
/// sound stack, so its shell has `beep`.
pub fn embed(builder: &mut bootloader::DiskImageBuilder, desktop: bool) {
    if desktop || enabled("LAZYOS_SOUND") {
        add(builder, "SNDD.ELF", "sndd");
        // `beep`, the smallest audio client: `sndd` spawns it under `demo=1`.
        add(builder, "BEEP.ELF", "beep");
        // `modplay`, the tracker-module player (docs/tracker-plan.md).
        add(builder, "MODPLAY.ELF", "modplay");
    }
    // `LAZYOS_NETD=1` adds the stack service and its tools, and needs the driver.
    let netd = enabled("LAZYOS_NETD");
    if netd || enabled("LAZYOS_NET") {
        add(builder, "NETDRV.ELF", "netdrv");
        // `nicctl` prints the card and carries the evidence clients the driver
        // spawns under `demo=1`.
        add(builder, "NICCTL.ELF", "nicctl");
    }
    if netd {
        add(builder, "NETD.ELF", "netd");
        // `netctl` and `ping`, the stack's shell commands and evidence clients.
        add(builder, "NETCTL.ELF", "netctl");
        add(builder, "PING.ELF", "ping");
        // `nc` and `nslookup`: sockets and name lookups (stage N3).
        add(builder, "NC.ELF", "nc");
        add(builder, "NSLOOKUP.ELF", "nslookup");
        // `ftp`, the passive-mode client (stage N4).
        add(builder, "FTP.ELF", "ftp");
        // `netfix`, the `std::net` Linux fixture the `AF_INET` shim is judged
        // by (stage N5), when the harness built one (`tools/abi/build.py`);
        // without a musl toolchain the image simply lacks it.
        println!("cargo:rerun-if-env-changed=LAZYOS_NETFIX");
        if let Some(path) = netfix() {
            println!("cargo:rerun-if-changed={}", path.display());
            builder.set_file(String::from("NETFIX.ELF"), path);
        }
    }
}
