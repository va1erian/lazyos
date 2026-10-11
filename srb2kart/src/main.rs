//! `lazysrb2kart`: SRB2Kart (Kart-Public v1.6) as a LazyOS desktop app.
//!
//! The engine (C, `libsrb2kart.a`) owns the game; this program owns the
//! process: it picks the data and config directories, chooses the backend
//! (headless now, a desktop window in K1) and implements the `lz_*` hooks the
//! engine's platform layer calls. See `docs/srb2kart-port-plan.md` and
//! `srb2kart/README.md`.

#[cfg(all(target_os = "linux", target_env = "musl"))]
mod engine;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod hooks;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod window;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod window_backend;
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
        "lazysrb2kart runs on LazyOS (x86_64-unknown-linux-musl); build it with tools/srb2kart/build.py"
    );
    std::process::exit(2);
}
