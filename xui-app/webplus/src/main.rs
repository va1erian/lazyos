//! `xui-lazywebplus`: LazyWeb+, the browser (docs/lazywebplus.md). On LazyOS the window runs as a
//! compositor client over webd's WebKit (`xui_webplus::platform`); elsewhere there is nothing to run
//! (the window is tested offscreen, `tests/`).

#[cfg(all(target_os = "linux", target_env = "musl"))]
fn main() {
    std::process::exit(xui_webplus::platform::main());
}

#[cfg(not(all(target_os = "linux", target_env = "musl")))]
fn main() {
    eprintln!("LazyWeb+ runs on LazyOS only (it is a client of webd)");
    std::process::exit(1);
}
