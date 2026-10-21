//! The seam between WebKit's UI process and a window on LazyOS (docs/webkit-port-plan.md, docs/lazywebplus.md).
//!
//! The engine is WebKit's UI-process half, in C++; this crate is everything on the xui side of it
//! that a window needs whichever window it is: the engine trait the window drives ([`engine`]),
//! the hand-over between the two threads ([`mailbox`], with [`events`] for the news that is not a
//! picture or the page's state), the page view that shows WebKit's bitmap and forwards the mouse,
//! wheel and keyboard ([`view`], [`keys`]), the pure parts of the C ABI ([`abi`]) and, on LazyOS,
//! the C ABI itself (`ffi`).
//!
//! The window that uses it is LazyWebPlus (`xui-app/webplus`), which brings the browser's chrome and
//! defines the per-window functions of the C ABI. The logic here is portable and tested on the
//! host against a recording [`engine`].

pub mod abi;
pub mod address;
pub mod bands;
pub mod damage;
pub mod engine;
pub mod events;
pub mod framing;
pub mod gesture;
pub mod glide;
pub mod keys;
pub mod mailbox;
pub mod parts;
mod picture;
pub mod pointer;
pub mod raster;
pub mod smooth;
pub mod speculate;
pub mod taskstat;
#[cfg(test)]
mod tests_damage;
#[cfg(test)]
mod tests_raster;
pub mod view;
mod view_input;

#[cfg(all(target_os = "linux", target_env = "musl"))]
pub mod ffi;
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod ffi_paint;
