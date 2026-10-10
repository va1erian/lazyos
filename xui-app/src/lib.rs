//! An ordinary `xui` application on LazyOS (issue #114).
//!
//! The crate is a `std` program built for `x86_64-unknown-linux-musl` (static):
//! `xui-core` drives the widgets, `xui-canvas` (a git dependency built with
//! `default-features = false`) paints them with
//! `tiny-skia` + `cosmic-text`, and [`backend::LazyOSBackend`] presents the
//! result through the native display grant (syscall 12) and turns the kernel's
//! input records back into `xui` events. Nothing here links `winit`,
//! `softbuffer` or GL.
//!
//! [`sysinfo`] and [`fabric`] are read-only native-syscall clients (14 and 5)
//! for the windowed system-state viewers (`sysmon`, `fabricmon`); [`services`]
//! joins `init`'s supervision table with `healthd`'s rows for `sysmon`'s
//! Services view. [`net`] backs the network apps (Network, Net Tools).

pub mod backend;
pub mod client_window;
pub mod compact;
pub mod dashboard;
pub mod devinfo;
pub mod display;
pub mod fabric;
pub mod font;
pub mod format;
pub mod hidpi;
pub mod input;
pub mod installer;
pub mod launch;
pub mod net;
pub mod platform;
pub mod probe;
pub mod resident;
pub mod server;
pub mod services;
pub mod shell;
pub mod stall;
pub mod sys;
/// The system-stats snapshot (syscall 14), shared with the native `top`.
pub use lazyos_sys::sysinfo;
pub mod themed;
pub mod tray;
