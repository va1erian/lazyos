//! Serial evidence markers of the LazyRAD player and IDE (AGENTS.md: a
//! graphics claim needs real output).
//!
//! The mechanism is `xui_app::trace` (docs/tracing-plan.md, T3): one whole
//! line written to the console device, so a program started from a Terminal
//! window still reaches the serial port, and compiled out by
//! `LAZYOS_TRACE=0`. This module only names the two programs' channels. The
//! format is `<PREFIX>:<STAGE>:<PASS|FAIL|WARN>[:<detail>]`; each one-shot
//! marker (`Markers::once`) fires at most once per process so a session
//! script can assert on it.

use xui_app::trace::Channel;
pub use xui_app::trace::Markers;

/// Markers for the `lrplay` player (`LRPLAY:`).
pub const PLAYER: Markers = Markers::new(Channel::Lrplay);
/// Markers for the `lazyrad` IDE (`LRIDE:`).
pub const IDE: Markers = Markers::new(Channel::Lride);
