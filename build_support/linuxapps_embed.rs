//! Embed the real Linux command-line programs (`LAZYOS_LINUXAPPS=1`).
//!
//! `tools/linuxapps/build.py` builds unmodified upstream programs (dash,
//! sqlite3, jq, ripgrep) as static `x86_64-linux-musl` executables into
//! `target/linuxapps/bin/<name>`. With the switch on, each one that exists is
//! stored at its `fhs::bin` path in `/system/bin`, where `sh` finds it by name
//! like any other program (`process/linux/path.rs` maps `rg` and `/bin/rg` to
//! `/system/bin/rg`). They are opt-in because together they add about 7 MiB.
//! Unlike `rhai`, they are embedded in ABI bench images too: the bench's
//! `linuxapps` row runs them.
//!
//! `LAZYOS_JSC=1` is the same mechanism for JavaScriptCore's `jsc` shell and
//! its C++/ICU check `cxxproof` (`tools/jsc/build.py`, built in Docker into
//! `target/jsc/bin/`; docs/webkit-port-plan.md W1 and W2). They are tens of MB
//! (ICU's data is inside) and LazyOS loads an ELF whole into RAM, so the
//! guest wants the `--big-app` profile.
//!
//! `LAZYOS_WEBKIT_DEPS=1` embeds `depsproof`, the C++ program that links the
//! whole WebKit dependency bundle (`tools/webkit/deps.py`, built in the same
//! container into `target/webkit/bin/`; docs/webkit-port-plan.md W3).
//!
//! `LAZYOS_WEBKIT_PORT=1` embeds the WebKit port's console proof (`tools/webkit/port.py`,
//! W4): the `WebProcess` and `NetworkProcess` it launches, the headless UI process
//! `webkit-smoke`, and the Fontconfig configuration they read (`webkit/port/fonts.conf` at
//! `fhs::etc::FONTS_CONF`). `LAZYOS_WEBKIT=1` is the browser (`webkit_embed`, W5): the same two
//! processes and configuration, without the proof, beside the app's core package.

use std::path::Path;

use crate::os_image::Sink;

/// Where `tools/linuxapps/build.py` writes the programs.
const BUILT: &str = "target/linuxapps/bin";

/// Every program the switch embeds: `(file name in BUILT, image path)`.
const PROGRAMS: &[(&str, &str)] = &[
    ("dash", fhs::bin::DASH),
    ("sqlite3", fhs::bin::SQLITE3),
    ("jq", fhs::bin::JQ),
    ("rg", fhs::bin::RG),
];

/// Where `tools/jsc/build.py` writes its programs, and what `LAZYOS_JSC=1` embeds.
const JSC_BUILT: &str = "target/jsc/bin";
const JSC_PROGRAMS: &[(&str, &str)] = &[
    ("jsc", fhs::bin::JSC),
    ("jsc-jit", fhs::bin::JSC_JIT),
    ("cxxproof", fhs::bin::CXXPROOF),
];
/// The stress-test bundle `tools/jsc/stress.py` writes, embedded at `fhs::share::JSC_TESTS`.
const JSC_TESTS_BUILT: &str = "target/jsc/jsc-tests";

/// Where `tools/webkit/deps.py` writes `depsproof`, and what `LAZYOS_WEBKIT_DEPS=1` embeds.
const WEBKIT_DEPS_BUILT: &str = "target/webkit/bin";
const WEBKIT_DEPS_PROGRAMS: &[(&str, &str)] = &[("depsproof", fhs::bin::DEPSPROOF)];

/// Where `tools/webkit/port.py` writes the port's programs, and what `LAZYOS_WEBKIT_PORT=1`
/// embeds; `LAZYOS_WEBKIT=1` embeds the first two (the browser starts them).
const WEBKIT_BUILT: &str = "target/webkit/bin";
const WEBKIT_PROCESSES: &[(&str, &str)] = &[
    ("WebProcess", fhs::bin::WEBPROCESS),
    ("NetworkProcess", fhs::bin::NETWORKPROCESS),
];
const WEBKIT_PORT_PROGRAMS: &[(&str, &str)] = &[
    ("WebProcess", fhs::bin::WEBPROCESS),
    ("NetworkProcess", fhs::bin::NETWORKPROCESS),
    ("webkit-smoke", fhs::bin::WEBKIT_SMOKE),
];
/// The Fontconfig configuration of those programs (a checked-in file).
const WEBKIT_FONTS_CONF: &str = "webkit/port/fonts.conf";
/// The Liberation fonts (`assets/manifest.txt` installs them in desktop images only): a
/// console image with the WebKit port has no other fonts, and a page without fonts has no text.
const WEBKIT_FONTS: &str = "assets/fonts/liberation";

/// Whether the environment variable `name` is `1`.
fn switch_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

/// Whether `LAZYOS_LINUXAPPS` asks for the programs.
pub fn enabled() -> bool {
    switch_on("LAZYOS_LINUXAPPS")
}

/// Add the built programs to `/system/bin` when their switch is on.
pub fn embed(sink: &mut dyn Sink, manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build_support/linuxapps_embed.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_LINUXAPPS");
    println!("cargo:rerun-if-env-changed=LAZYOS_JSC");
    println!("cargo:rerun-if-env-changed=LAZYOS_WEBKIT_DEPS");
    println!("cargo:rerun-if-env-changed=LAZYOS_WEBKIT");
    println!("cargo:rerun-if-env-changed=LAZYOS_WEBKIT_PORT");
    if enabled() {
        embed_programs(
            sink,
            manifest_dir,
            "LAZYOS_LINUXAPPS",
            BUILT,
            PROGRAMS,
            "tools/linuxapps/build.py",
        );
    }
    if switch_on("LAZYOS_JSC") {
        embed_programs(
            sink,
            manifest_dir,
            "LAZYOS_JSC",
            JSC_BUILT,
            JSC_PROGRAMS,
            "tools/jsc/build.py",
        );
        embed_tree(
            sink,
            &manifest_dir.join(JSC_TESTS_BUILT),
            fhs::share::JSC_TESTS,
        );
    }
    if switch_on("LAZYOS_WEBKIT_DEPS") {
        embed_programs(
            sink,
            manifest_dir,
            "LAZYOS_WEBKIT_DEPS",
            WEBKIT_DEPS_BUILT,
            WEBKIT_DEPS_PROGRAMS,
            "tools/webkit/deps.py",
        );
    }
    for (switch, programs) in [
        ("LAZYOS_WEBKIT_PORT", WEBKIT_PORT_PROGRAMS),
        ("LAZYOS_WEBKIT", WEBKIT_PROCESSES),
    ] {
        if !switch_on(switch) {
            continue;
        }
        embed_programs(
            sink,
            manifest_dir,
            switch,
            WEBKIT_BUILT,
            programs,
            "tools/webkit/port.py",
        );
        let conf = manifest_dir.join(WEBKIT_FONTS_CONF);
        println!("cargo:rerun-if-changed={}", conf.display());
        sink.add_file(fhs::etc::FONTS_CONF, conf);
        embed_fonts(sink, &manifest_dir.join(WEBKIT_FONTS));
    }
}

/// Store every `.ttf` of `dir` under `fhs::share::FONTS_LIBERATION` (a desktop image already
/// has them there; the later entry replaces the earlier).
fn embed_fonts(sink: &mut dyn Sink, dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = std::fs::read_dir(dir) else {
        println!("cargo:warning=LAZYOS_WEBKIT: no fonts in {}", dir.display());
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| name.ends_with(".ttf")) else {
            continue;
        };
        sink.add_file(
            &format!("{}/{name}", fhs::share::FONTS_LIBERATION),
            entry.path(),
        );
    }
}

/// Store every regular file directly under `dir` (when it exists) at
/// `image_dir/<name>`. The directory is watched, so a new bundle rebuilds the image.
fn embed_tree(sink: &mut dyn Sink, dir: &Path, image_dir: &str) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = std::fs::read_dir(dir) else {
        println!(
            "cargo:warning=LAZYOS_JSC: {} not built, no stress tests (run tools/jsc/stress.py)",
            dir.display()
        );
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .collect();
    files.sort_by_key(|entry| entry.file_name());
    for entry in files {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        println!("cargo:rerun-if-changed={}", entry.path().display());
        sink.add_file(&format!("{image_dir}/{name}"), entry.path());
    }
}

/// Store each program of `programs` that `built_dir` holds at its image path.
fn embed_programs(
    sink: &mut dyn Sink,
    manifest_dir: &Path,
    switch: &str,
    built_dir: &str,
    programs: &[(&str, &str)],
    builder: &str,
) {
    let dir = manifest_dir.join(built_dir);
    let mut missing = Vec::new();
    for (file, image_path) in programs {
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
        println!("cargo:warning={switch} embedded: {}", dir.display());
    } else {
        println!(
            "cargo:warning={switch}: not built, left out: {} (run {builder})",
            missing.join(", ")
        );
    }
}
