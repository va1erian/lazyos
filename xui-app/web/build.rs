//! This build script exists only to make cargo honour the `cc`
//! build-dependency declared in `Cargo.toml`: enabling `cc`'s `parallel`
//! feature unifies it across the host build-dependency graph, so
//! `netsurf-sys`'s C files compile concurrently instead of one at a time.

// Named so cargo sees the dependency as used.
use cc as _;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
