//! The platform-independent half of the SRB2Kart port
//! (`docs/srb2kart-port-plan.md`): everything that decides *what* to hand the
//! engine, kept free of syscalls so it runs under plain `cargo test` on the
//! host.
//!
//! - [`launch`]: the command line, the data and home directories;
//! - [`backend`]: the trait the C seam (`hooks.rs`) drives, its event and
//!   frame types;
//! - [`headless`]: the backend with no display, a virtual clock and checksummed
//!   frames;
//! - [`crc`]: the frame checksum the headless mode reports.
//!
//! The window half (`pixels`, `keymap`, `keys`, `session`, `window`) joins
//! here in K1. The binary (`main.rs`) adds the `lz_*` hooks and the engine.

pub mod backend;
pub mod crc;
pub mod headless;
pub mod launch;
