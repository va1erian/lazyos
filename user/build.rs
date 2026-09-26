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
}
