//! Embed the real Linux command-line programs (`LAZYOS_LINUXAPPS=1`).
//!
//! `tools/linuxapps/build.py` builds unmodified upstream programs (dash, lua,
//! sqlite3, jq, ripgrep) as static `x86_64-linux-musl` executables into
//! `target/linuxapps/bin/<name>`. With the switch on, each one that exists is
//! stored at its `fhs::bin` path in `/system/bin`, where `sh` finds it by name
//! like any other program (`process/linux/path.rs` maps `rg` and `/bin/rg` to
//! `/system/bin/rg`). They are opt-in because together they add about 7 MiB.
//! Unlike `rhai`, they are embedded in ABI bench images too: the bench's
//! `linuxapps` row runs them.

use std::path::Path;

use crate::os_image::Sink;

/// Where `tools/linuxapps/build.py` writes the programs.
const BUILT: &str = "target/linuxapps/bin";

/// Every program the switch embeds: `(file name in BUILT, image path)`.
const PROGRAMS: &[(&str, &str)] = &[
    ("dash", fhs::bin::DASH),
    ("lua", fhs::bin::LUA),
    ("sqlite3", fhs::bin::SQLITE3),
    ("jq", fhs::bin::JQ),
    ("rg", fhs::bin::RG),
];

/// Whether `LAZYOS_LINUXAPPS` asks for the programs.
pub fn enabled() -> bool {
    std::env::var("LAZYOS_LINUXAPPS").is_ok_and(|value| value == "1")
}

/// Add the built programs to `/system/bin` when the switch is on.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/linuxapps_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_LINUXAPPS");
    if !enabled() {
        return;
    }
    let dir = manifest_dir.join(BUILT);
    let mut missing = Vec::new();
    for (file, image_path) in PROGRAMS {
        let built = dir.join(file);
        // Watched even when missing: a program built later rebuilds the image.
        println!("cargo:rerun-if-changed={}", built.display());
        if built.is_file() {
            sink.add_file(image_path, built);
        } else {
            missing.push(*file);
        }
    }
    if missing.is_empty() {
        println!("cargo:warning=LAZYOS_LINUXAPPS embedded: {}", dir.display());
    } else {
        println!(
            "cargo:warning=LAZYOS_LINUXAPPS: not built, left out: {} (run tools/linuxapps/build.py)",
            missing.join(", ")
        );
    }
}
