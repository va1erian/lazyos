//! webd's service half (docs/webd-plan.md, phase P2 slice 1): the Messenger service that gives apps
//! WebKit views, here the light ones (`os.lazy.webd.light.v1`, `idl/webd_light.midl`).
//!
//! The crate is portable and host-tested: it knows nothing of xui, of the kernel or of WebKit. The
//! program that hosts it (slice 2) supplies
//!
//! * a [`fabric::Fabric`] over the kernel's channels and buffers, and each request's kernel-stamped
//!   [`ident::Origin`];
//! * an [`engine::Engine`] (the `ffi` module, feature `host`, over the C ABI of
//!   `webkit/port/webd/webd_abi.h`) and the [`events::EventQueue`] the engine's callbacks fill;
//! * the traces: [`service::Service::take_notes`] returns what happened, the host prints it.
//!
//! Layout:
//!
//! * [`limits`]: every number enforced on a client.
//! * [`ident`]: who is calling and the web process group of each client.
//! * [`config`]: validation of sizes, `LightConfig`, base URLs.
//! * [`slots`]: the slot state machine of a view; [`view`]: the table of views and their owners.
//! * [`service`]: the dispatch of client calls and the delivery of the engine's news.
//! * [`engine`], [`events`], [`recording`]: the engine seam, its thread hand-over and a test double.
//! * [`fabric`], [`mem`]: the Messenger seam and an in-memory fabric for tests.
//!
//! The security properties, each tested: a view is usable only by the (user, package) that made it
//! and looks nonexistent to anyone else; web process groups are chosen here, never by a client and
//! never shared between clients; a light view's configuration has four fields and no way to ask for a
//! network, a profile or a scheme; a client that stops reading loses frames and nothing else; a
//! client that dies loses its views.

pub mod config;
pub mod engine;
pub mod events;
pub mod fabric;
pub mod fullconfig;
pub mod ident;
pub mod limits;
pub mod mem;
pub mod outside;
pub mod policy;
pub mod recording;
mod reply;
pub mod service;
pub mod slots;
pub mod view;

#[cfg(feature = "host")]
pub mod ffi;

pub use service::{Call, Note, Service};
