//! `lazyquake`: Quake (the `quake-srp` port of id's WinQuake, GPL-2.0-or-
//! later) as a LazyOS desktop app.
//!
//! The engine's platform is quake-wasm's, byte for byte
//! (`docs/quake-port-plan.md`): `common::init` (the search path),
//! `app::ensure_app` (the settings, the presets for this machine), and
//! `sys::run` — the program's loop, whose page protocol this process is
//! ([`lazy::bridge`]). The differences from the browser are the platform
//! side only:
//!
//! - the search path opens the package's read-only resources
//!   (`-basedir <install>/resources`, the shareware pak);
//! - the data directory is the package's own folder in the player's home
//!   ([`lazy::launch`], told to `common` before the first frame);
//! - no gamepad ever exists (`-nojoy`), and the window's key records feed
//!   the input instead of the page's.
//!
//! Modes: a desktop window (the default, `--client` accepted for
//! `init`'s manifest `entry.args`) and a headless harness
//! (`-headless -frames N`), as the Doom port's.

pub mod lazy;

// The engine's own platform layer (`quake-wasm`), every module declared
// exactly as its upstream root does (same `cfg(test)` gates, so the
// upstream tests run on a host build too).
mod app;
mod automation;
mod bench;
mod cl_demo;
#[cfg(test)]
mod cl_tent;
mod cl_walk;
mod common;
mod config;
mod console;
mod host;
mod host_cmd;
mod input;
mod menu;
#[cfg(test)]
mod oracle_screen;
mod present;
mod proto;
mod savegame;
mod snd_dma;
mod sys;
#[cfg(test)]
mod test_util;
mod vid;

#[cfg(test)]
#[path = "census_tests.rs"]
mod census_tests;
#[cfg(test)]
mod content_tests;
#[cfg(test)]
mod nail_tests;

fn main() -> std::process::ExitCode {
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    return lazyos_main();
    #[cfg(not(all(target_os = "linux", target_env = "musl")))]
    return host_stub();
}

/// The program on the OS (the mode the shipped binary is built in).
#[cfg(all(target_os = "linux", target_env = "musl"))]
fn lazyos_main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let args: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string());
    let launch = lazy::launch::parse(&args, &cwd);
    let exe = lazy::launch::exe_path(args.first().map(String::as_str).unwrap_or(""), &cwd);
    // `COM_InitFilesystem`'s `-rogue`/`-hipnotic`/`-game <dir>` still
    // pass through, in id's order.
    let (dirs, force_modified) = mod_dirs(&args);
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let basedir = lazy::launch::resolve_basedir(&launch, &exe);
    let log = match crate::common::init(std::path::Path::new(&basedir), &dirs, force_modified) {
        Ok(log) => log,
        Err(e) => {
            // A missing pak fails softly, with one clear line.
            println!("QUAKE:PAK:FAIL {basedir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The search path found id's pak; the saves and `config.cfg` go to
    // the package's per-user folder in the player's home. A folder the
    // platform cannot create is a launch failure, not a soft one: the
    // engine's fallback would write into the package's own (read-only,
    // replaced on update) tree, nothing of it would survive.
    let home = std::env::var("HOME").ok();
    let data_dir = lazy::launch::config_dir(home.as_deref());
    match std::fs::create_dir_all(&data_dir) {
        Ok(()) => crate::common::set_data_dir(std::path::PathBuf::from(&data_dir)),
        Err(e) => {
            println!("QUAKE:DATA:FAIL cannot create {data_dir}: {e}");
            return ExitCode::FAILURE;
        }
    }
    // ID_StartupClient: the presets' numbers for this machine, before
    // `quake.rc` runs.
    let machine = machine(&args);
    crate::app::ensure_app(|a| {
        a.settings = quake_rs::settings::Settings::new(crate::app::START_PRESET, machine);
        a.present = crate::present::Present::new(false);
        for line in log {
            a.console.println(line);
        }
        // Not notify lines: the attract demo's signon ends in
        // SCR_EndLoadingPlaque's Con_ClearNotify before anything is drawn.
        let _ = a.console.take_unnotified();
        // `IN_StartupJoystick`'s `-nojoy`: no pad exists on v1.
        a.pad.joy.set_nojoy();
    });
    match launch.mode {
        lazy::launch::Mode::Window => lazy::run_window(launch),
        lazy::launch::Mode::Headless { frames } => lazy::run_headless(launch, frames),
    }
}

/// The machine the presets' numbers are built for
/// ([`quake_rs::settings::Machine`]): the threads the host offers,
/// `-hwthreads <n>` overriding (`id'S `IN_...`, the page's "-hwthreads").
#[cfg(all(target_os = "linux", target_env = "musl"))]
fn machine(args: &[String]) -> quake_rs::settings::Machine {
    let threads = args
        .iter()
        .position(|a| a == "-hwthreads")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
        .max(1);
    quake_rs::settings::Machine { touch: false, threads }
}

/// `COM_InitFilesystem`'s `-rogue`/`-hipnotic`/`-game <dir>`: the game
/// directories to layer over `id1`, in id's order (`rogue`, then
/// `hipnotic`, then `-game`'s own directory), and whether `-game` was
/// given (it forces `com_modified`, id's way).
#[cfg(all(target_os = "linux", target_env = "musl"))]
fn mod_dirs(args: &[String]) -> (Vec<String>, bool) {
    let mut dirs = Vec::new();
    if args.iter().any(|a| a == "-rogue") {
        dirs.push("rogue".to_string());
    }
    if args.iter().any(|a| a == "-hipnotic") {
        dirs.push("hipnotic".to_string());
    }
    let game_dir = args.iter().position(|a| a == "-game").and_then(|i| args.get(i + 1));
    if let Some(dir) = game_dir {
        dirs.push(dir.clone());
    }
    (dirs, game_dir.is_some())
}

/// Quake only runs on LazyOS (x86_64-unknown-linux-musl); a host build is
/// for `cargo test` (the upstream suite over the record protocol), which
/// imports `crate::sys` directly.
#[cfg(not(all(target_os = "linux", target_env = "musl")))]
fn host_stub() -> std::process::ExitCode {
    eprintln!(
        "lazyquake runs on LazyOS (x86_64-unknown-linux-musl); build it with tools/quake/build.py"
    );
    std::process::exit(2)
}
