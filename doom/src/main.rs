//! `lazydoom`: Doom (doomgeneric + Freedoom) as a LazyOS desktop app.
//!
//! The engine (C, `libdoomgeneric.a`) owns the game; this program owns the
//! process: it picks the IWAD and the config directory, opens the window (or
//! runs headless), and implements the six `DG_*` hooks the engine calls. See
//! `docs/doom-port-plan.md` and `doom/README.md`.

#[cfg(all(target_os = "linux", target_env = "musl"))]
mod engine;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod headless;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod hooks;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod window;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod window_input;

#[cfg(all(target_os = "linux", target_env = "musl"))]
fn main() {
    engine::run();
}

/// The game only runs on LazyOS; host builds exist for `cargo test`.
#[cfg(not(all(target_os = "linux", target_env = "musl")))]
fn main() {
    eprintln!(
        "lazydoom runs on LazyOS (x86_64-unknown-linux-musl); build it with tools/doom/build.py"
    );
    std::process::exit(2);
}
