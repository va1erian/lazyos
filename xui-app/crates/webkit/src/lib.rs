//! WebKit's window on LazyOS (docs/webkit-port-plan.md W5).
//!
//! The engine is WebKit's UI-process half, in C++; this crate is everything on
//! the xui side of it: the toolbar and address field ([`chrome`], [`app`]), the
//! page view that shows WebKit's bitmap and forwards the mouse, wheel and
//! keyboard ([`view`], [`keys`]), the hand-over between the two threads
//! ([`mailbox`]), and on LazyOS the C ABI the C++ side calls (`ffi`). The window
//! logic is portable and tested on the host against a recording [`engine`].

pub mod address;
pub mod app;
pub mod chrome;
pub mod engine;
pub mod keys;
pub mod mailbox;
pub mod view;

#[cfg(all(target_os = "linux", target_env = "musl"))]
mod ffi;
