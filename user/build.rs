use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    // Link the program as a static (non-PIE) ELF64 at a fixed base with our own
    // entry point.
    println!("cargo:rustc-link-arg=-T{manifest}/link.ld");
    println!("cargo:rustc-link-arg=-no-pie");
    println!("cargo:rustc-link-arg=-static");
    println!("cargo:rerun-if-changed=link.ld");
    println!("cargo:rerun-if-changed=build.rs");

    // Desktop profile (issue #217): `LAZYOS_DESKTOP=1` marks the image as a
    // user-facing desktop. `init` reads `cfg(lazyos_desktop)` to keep the
    // demo/evidence-only programs (the crash test, the clipboard demo pair, the
    // `top` launch self-test) out of the boot, so the profile's behavior is
    // decided where the programs live, not only in the build scripts.
    println!("cargo:rerun-if-env-changed=LAZYOS_DESKTOP");
    println!("cargo:rustc-check-cfg=cfg(lazyos_desktop)");
    if env::var_os("LAZYOS_DESKTOP").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=lazyos_desktop");
    }
}
