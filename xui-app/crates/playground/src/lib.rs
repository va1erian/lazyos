//! The Messenger Playground's portable core (docs/messenger-playground-plan.md).
//!
//! No xui and no LazyOS types: the catalog of interfaces and topics, the doc
//! text, the Rhai snippet and form generators, the value pretty-printer, the
//! listener's accounting and the runner's bookkeeping are pure functions over
//! `rhai-lazy`'s schema table, tested on the host. `src/bin/playground.rs`
//! supplies the window and the LazyOS platform.

pub mod arrival;
pub mod catalog;
pub mod docs;
pub mod form;
pub mod form_parse;
pub mod gallery;
pub mod listener;
pub mod listener_decode;
pub mod listener_event;
pub mod live;
pub mod prelude;
pub mod rhai_names;
pub mod run_map;
pub mod runner;
pub mod runner_fake;
pub mod show;
pub mod snippet;
pub mod util;

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;
