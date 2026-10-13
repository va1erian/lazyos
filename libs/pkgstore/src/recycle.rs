//! When an idle `pkgd` ends itself to give its heap back.
//!
//! The user heap never reuses a block over 1 MiB, and reading a package holds the whole
//! archive in one buffer that `pkgd` keeps and reuses (sized to the largest package seen). So
//! a provisioning pass grows the heap by about the largest package, whatever the set. `pkgd`
//! ends itself (`init` supervises it with `Restart::Always` and starts a fresh one) once its
//! heap has grown past [`RECYCLE_BYTES`] *beyond that buffer*: without the allowance a package
//! over [`RECYCLE_BYTES`] (the WebKit browser's is 44 MB) made every fresh `pkgd` size its
//! buffer, find itself over the limit after one step and restart, and five rapid restarts
//! exhaust `init`'s budget, so provisioning stopped half way.
//!
//! The rule is here, not in the binary, so the host tests (`tests/provision_memory.rs`) use
//! the one `pkgd` does.

/// Heap growth, beyond the kept package buffer, after which an idle `pkgd` restarts.
pub const RECYCLE_BYTES: u64 = 32 * 1024 * 1024;

/// How far the heap may have grown, with `buffer_capacity` bytes held by the package buffer.
pub const fn allowed(buffer_capacity: u64) -> u64 {
    RECYCLE_BYTES.saturating_add(buffer_capacity)
}

/// Whether a `pkgd` whose heap grew by `grown` bytes since it started, with a package buffer of
/// `buffer_capacity` bytes, should restart (when idle between two requests).
pub const fn due(grown: u64, buffer_capacity: u64) -> bool {
    grown > allowed(buffer_capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_pass_never_restarts() {
        assert!(!due(0, 0));
        assert!(!due(RECYCLE_BYTES, 0));
        assert!(due(RECYCLE_BYTES + 1, 0));
    }

    #[test]
    fn the_package_buffer_is_not_counted_against_the_threshold() {
        // The WebKit browser's package: a 44 MB buffer and little else.
        let buffer = 44 * 1024 * 1024;
        assert!(!due(buffer + 1024 * 1024, buffer));
        assert!(due(buffer + RECYCLE_BYTES + 1, buffer));
        // A rule that ignored the buffer would restart a fresh pkgd after its first step.
        assert!(buffer > RECYCLE_BYTES);
    }

    #[test]
    fn the_allowance_cannot_overflow() {
        assert_eq!(allowed(u64::MAX), u64::MAX);
        assert!(!due(u64::MAX, u64::MAX));
    }
}
