//! The seam between WebKit and a window on LazyOS (docs/lazyweb.md, docs/webd.md).
//!
//! WebKit runs in `webd`, another process; this crate is everything on the xui side that a window
//! needs whichever engine it is driven through: the engine trait the window drives ([`engine`]),
//! the hand-over to the window's thread ([`mailbox`], with [`events`] for the news that is not a
//! picture or the page's state), the page view that shows the picture and forwards the mouse,
//! wheel and keyboard ([`view`], [`keys`]), the values both sides agree on ([`abi`]) and an engine
//! for other threads ([`relay`]).
//!
//! The window that uses it is LazyWeb (`xui-app/lazyweb`), over `xui-webkit-remote`'s
//! `RemoteEngine` (webd's full views). The logic here is portable and tested on the host against a
//! recording [`engine`].

pub mod abi;
pub mod address;
pub mod bands;
/// What changed between two paints: its own crate (`lazydamage`) so webd keeps the same rules per view.
pub use lazydamage as damage;
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
pub mod relay;
pub mod smooth;
pub mod speculate;
#[cfg(test)]
mod tests_raster;
pub mod view;
mod view_input;
