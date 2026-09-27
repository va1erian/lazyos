//! Shared reporting for ABI fixtures.
//!
//! Convention (parsed by `tools/abi/run.py`):
//!   `ABI:<name>:PASS`  or  `ABI:<name>:FAIL:<reason>`
//! The process exits 0 on pass, 1 on failure.

#![allow(dead_code)] // Each fixture uses a different subset.

/// Report success and let the program exit 0.
pub fn pass(name: &str) {
    println!("ABI:{name}:PASS");
}

/// Report failure and exit 1.
pub fn fail(name: &str, reason: &str) -> ! {
    println!("ABI:{name}:FAIL:{reason}");
    std::process::exit(1);
}

/// Report `ok ? PASS : FAIL(reason)`.
pub fn report(name: &str, ok: bool, reason: &str) {
    if ok {
        pass(name);
    } else {
        fail(name, reason);
    }
}
