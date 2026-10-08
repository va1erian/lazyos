//! `lazyemusic`: emusic on LazyOS. See the library crate's docs.

#[cfg(all(target_os = "linux", target_env = "musl"))]
fn main() {
    lazyemusic::app::run();
}

/// emusic runs on LazyOS; host builds exist for `cargo test`.
#[cfg(not(all(target_os = "linux", target_env = "musl")))]
fn main() {
    eprintln!(
        "lazyemusic runs on LazyOS (x86_64-unknown-linux-musl); build it with tools/emusic/build.py"
    );
    std::process::exit(2);
}
