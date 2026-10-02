//! Kernel-wide error type and the single fatal stop path.
//!
//! Two distinct situations get confused when every fallible kernel operation
//! reaches for `.unwrap()`/`.expect()`:
//!
//! 1. A condition that has a caller able to make a policy decision (deny a
//!    syscall, reclaim memory, retry, degrade a service) — this should be a
//!    `Result<T, KError>` that propagates to that caller, never a panic.
//! 2. A condition with provably no such caller (boot-time setup before the
//!    scheduler exists, a corrupted structure that makes forward progress
//!    unsound) — this is a genuine "the kernel cannot continue" situation.
//!
//! [`KError`] gives (1) a small, `Debug`/`Display`-able, `no_std` error type
//! instead of a bag of ad-hoc `&'static str` messages, so call sites can
//! match on *kind* rather than parse text. [`kstop`] is the single choke
//! point for (2): every genuinely unrecoverable condition in the kernel
//! reports through it instead of scattering `.expect("...")`/`panic!("...")`
//! call sites that each reinvent the "print and halt" sequence. It is
//! distinct from `panic!`: a panic means the kernel hit a bug (an invariant
//! it believed but didn't verify), `kstop` means the kernel correctly
//! detected a condition it has no policy for yet and is stopping cleanly
//! instead of continuing on unsound state.
//!
//! Call [`kstop`] only when no `Result`-returning alternative exists for the
//! call site (see `docs/architecture` and issue #123 for the audit that
//! motivated this). Anywhere a caller *could* receive a `KError` and act on
//! it, return the `Result` instead.

use core::fmt;

use crate::halt;

/// Kernel-wide error kind. Deliberately small and closed: new variants are
/// added as new subsystems need to report failures a caller can act on, not
/// as a generic catch-all.
#[allow(dead_code)] // Small, deliberately closed API surface; variants land as callers need them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KError {
    /// No physical frames are available to satisfy the request.
    OutOfMemory,
    /// A resource limit (quota, table capacity, handle count) was reached.
    ResourceExhausted,
    /// The requested object, path, or handle does not exist.
    NotFound,
    /// The caller does not hold the rights/permissions the operation needs.
    PermissionDenied,
    /// An argument or on-disk/on-wire structure failed validation.
    InvalidInput,
    /// The subsystem is not in a state that allows this operation right now.
    InvalidState,
    /// A lower-level operation (paging, device I/O) failed.
    Io,
}

impl KError {
    /// A short machine-stable name, for log lines and `messengerctl`-style
    /// introspection that wants to match on error kind, not parse prose.
    pub const fn as_str(self) -> &'static str {
        match self {
            KError::OutOfMemory => "out-of-memory",
            KError::ResourceExhausted => "resource-exhausted",
            KError::NotFound => "not-found",
            KError::PermissionDenied => "permission-denied",
            KError::InvalidInput => "invalid-input",
            KError::InvalidState => "invalid-state",
            KError::Io => "io-error",
        }
    }
}

impl fmt::Display for KError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Convenience alias for kernel code returning a [`KError`].
#[allow(dead_code)] // Not yet used by a converted call site; wired in incrementally.
pub type KResult<T> = Result<T, KError>;

/// The single fatal choke point for conditions the kernel has no caller able
/// to recover from — use only when no `Result`-returning path exists back to
/// a caller that could act on the failure (see the module docs).
///
/// This is not `panic!`: reaching `kstop` means the kernel correctly
/// identified an unrecoverable condition and is stopping deliberately, with
/// a structured [`KError`] and a short, specific reason, rather than
/// continuing on unsound state. Every call site should read like a sentence:
/// `kstop(KError::OutOfMemory, "no room for the frame refcount table")`.
#[cold]
pub fn kstop(error: KError, context: &str) -> ! {
    crate::serial_println!("LazyOS KSTOP [{}]: {}", error.as_str(), context);
    crate::panic_screen::show(
        "LazyOS stopped (kstop)",
        format_args!("[{}] {}", error.as_str(), context),
    );
    halt();
}
