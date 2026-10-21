//! The client of webd's light views (docs/webd-plan.md, phase P2 slice 2): an app shows formatted
//! text without a browser. [`LightWebView`] asks `webd` for a view over `os.lazy.webd.light.v1`
//! (`idl/webd_light.midl`), gives it shared slots to paint into, loads the app's HTML, and turns the
//! events that come back into a persistent [`Picture`] the app draws and a few facts about the page
//! (title, scroll, link clicks, copied text).
//!
//! The crate is portable and host-tested: it knows nothing of xui, of the kernel or of WebKit. The
//! program that hosts it supplies a [`Transport`] over the kernel's channels and buffers
//! (`xui_app::platform::webd`); the tests supply one over the service half's in-memory fabric.
//!
//! Layout:
//!
//! * [`transport`]: the OS seam. [`view`]: the view, its slots and its events. [`input`]: oneway
//!   input. [`event`]: the events, errors and the reasons a view is lost. [`picture`]: the picture.
//! * [`page`], [`demo`]: the HTML and the logic of the demo client (`webddemo`), apart from its window, so
//!   the host tests and the QEMU harness (`tools/webd`) agree with the app.
//!
//! What a client sees when things go wrong, each tested: a refused call is [`Error::Refused`] with
//! the service's errno and leaves the view as it was; a service that is not there is
//! [`Error::NotRunning`] on create; a service or view that goes away mid-life is
//! [`ViewEvent::Lost`] (from `poll`) and [`Error::Lost`] (from any later call), never a panic or a
//! hang; [`LightWebView::recreate`] makes a new view and loads the last page again.

pub mod demo;
pub mod event;
pub mod input;
pub mod page;
pub mod picture;
pub mod transport;
pub mod view;

pub use event::{Error, Lost, PageState, ViewEvent};
pub use input::{mods, MouseKind};
pub use picture::{Colour, Picture};
pub use transport::{SharedMem, Transport, SERVICE_NAME};
pub use view::{Counters, LightConfig, LightWebView, EVENTS_PER_POLL, SLOTS};
