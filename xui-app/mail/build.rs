//! This build script exists only to make cargo honour the `cc`
//! build-dependency declared in `Cargo.toml`: enabling `cc`'s `parallel`
//! feature here unifies it across the host build-dependency graph, so
//! `litehtml-sys`'s ~80 C/C++ files (and SQLite) compile concurrently instead of one at a
//! time. It has no work of its own; declaring itself as its only input keeps it
//! from rerunning needlessly.

use cc as _;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
