//! Host tests: `cargo test -p lazyos-install`. No root, no disk: test mode
//! (`--image`) runs every command against files in a temporary directory.

mod e2e;
mod fixture;
mod grubenv_tests;
mod plan_tests;
