//! The host of webd's service (docs/webd.md; docs/webd-plan.md, P2 slice 2).
//!
//! `webd-app` is the C++ program of `webkit/port/webd/main.cpp`: WebKit's run loop on a thread of
//! its own, and this library on the process's first thread. The first thread is the one `init` knows
//! the app by (a lifecycle `Watch` from any other thread is `ESRCH`, as the shell's notification
//! service refuses one), so the service, the app's line to `init` and the registry name live on it,
//! and the engine, whose calls only queue work, lives on the other.
//!
//! What this crate adds to `xui-webd` (the portable service):
//!
//! * [`fabric`]: the real Messenger binding: [`xui_webd::fabric::Fabric`] over the kernel's channels
//!   and shared buffers, and the request/reply loop over [`xui_app::server::Server`].
//! * [`doorbell`]: an `eventfd` the engine's thread rings when it queues news, which the service
//!   thread parks on together with its endpoint and lifecycle channel ([`serve`]).
//! * [`heartbeat`]: the watchdog that notices a WebKit main thread that has stopped turning.
//! * [`lines`]: the notes of the service as the `WEBD:*` markers a harness reads.
//! * [`selftest`]: webd's built-in client, which runs the service through the real road in an image with
//!   no other client (`Launch os.lazy.webd` with `/selftest`).
//! * [`exports`]: the C functions `main.cpp` calls.
//!
//! The pure parts ([`lines`], [`heartbeat`]) build and test on any host; the rest is LazyOS's
//! (`x86_64-unknown-linux-musl`).

pub mod heartbeat;
pub mod lines;

#[cfg(all(target_os = "linux", target_env = "musl"))]
mod doorbell;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod exports;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod fabric;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod selftest;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod serve;
