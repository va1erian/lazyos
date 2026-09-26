//! Launches the built disk image in QEMU.

use std::env;
use std::process::Command;

/// Path produced by `build.rs`.
const BIOS_IMAGE: &str = env!("BIOS_IMAGE");

fn main() {
    let headless = env::args().any(|arg| arg == "--headless");
    let qemu = env::var("QEMU").unwrap_or_else(|_| "qemu-system-x86_64".to_string());

    let mut cmd = Command::new(&qemu);
    cmd.arg("-drive")
        .arg(format!("format=raw,file={BIOS_IMAGE}"));
    cmd.arg("-m").arg("256M");
    cmd.arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
    if headless {
        cmd.arg("-display").arg("none");
        cmd.arg("-serial").arg("stdio");
    } else {
        cmd.arg("-serial").arg("mon:stdio");
    }

    let status = cmd.status().expect("failed to start qemu-system-x86_64");
    // `isa-debug-exit` maps the guest's written value through `(value << 1) | 1`.
    match status.code().unwrap_or(1) {
        0x10 => std::process::exit(0), // success
        0x11 => std::process::exit(1), // failure
        _ => std::process::exit(2),    // unknown fault
    }
}
