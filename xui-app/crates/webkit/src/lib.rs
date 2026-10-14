//! The seam between WebKit's UI process and a window on LazyOS (docs/webkit-port-plan.md W5).
//!
//! The engine is WebKit's UI-process half, in C++; this crate is everything on the xui side of it
//! that a window needs whichever window it is: the engine trait the window drives ([`engine`]),
//! the hand-over between the two threads ([`mailbox`], with [`events`] for the news that is not a
//! picture or the page's state), the page view that shows WebKit's bitmap and forwards the mouse,
//! wheel and keyboard ([`view`], [`keys`]), the pure parts of the C ABI ([`abi`]) and, on LazyOS,
//! the C ABI itself (`ffi`).
//!
//! Two windows use it. With the default feature `window` this crate is `os.lazy.webkit`'s
//! window as well ([`app`], [`chrome`], `window_ffi`: a toolbar, an address field and a status
//! line). LazyWebPlus (`xui-app/webplus`) turns that off, links the rest and brings LazyWeb's
//! chrome. The window logic is portable and tested on the host against a recording [`engine`].

pub mod abi;
pub mod address;
pub mod engine;
pub mod events;
pub mod gesture;
pub mod keys;
pub mod mailbox;
pub mod taskstat;
pub mod view;

#[cfg(feature = "window")]
pub mod app;
#[cfg(feature = "window")]
pub mod chrome;

#[cfg(all(target_os = "linux", target_env = "musl"))]
pub mod ffi;
#[cfg(all(target_os = "linux", target_env = "musl", feature = "window"))]
mod window_ffi;
