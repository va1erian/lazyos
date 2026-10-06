//! The platform-independent half of the Doom port (`docs/doom-port-plan.md`):
//! everything that decides *what* to hand the engine, kept free of syscalls so
//! it runs under plain `cargo test` on the host.
//!
//! - [`keymap`]: LazyOS key events (the `inputd` session's HID usages and
//!   keysyms, or the compositor's legacy key codes) to doomgeneric key codes;
//! - [`keys`]: the edge-tracking queue `DG_GetKey` drains (no auto-repeat
//!   presses, every held key released on focus loss);
//! - [`pixels`]: the engine's XRGB framebuffer to the compositor's RGBA,
//!   scaled to the window with the aspect ratio kept;
//! - [`launch`]: the command line, the install directory and the IWAD path;
//! - [`crc`]: the frame checksum the headless mode reports;
//! - [`session`]: the `inputd` key-state page as the authority on held keys,
//!   and the keyboard grab taken while the window is maximized (I3).
//!
//! The binary (`main.rs`) adds the `DG_*` hooks, the window and the engine.

pub mod crc;
pub mod keymap;
pub mod keys;
pub mod launch;
pub mod pixels;
pub mod session;
