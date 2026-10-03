#![forbid(unsafe_code)]

//! LazyWriter (issue #533): a word processor on `xui-rich-text`, ported from
//! xui's `crates/xui-rich-text/examples/wordpad`.
//!
//! The crate holds everything portable: the app state and messages
//! ([`app`]), the commands ([`commands`]), the file logic ([`files`],
//! [`names`], [`probe`]) and the widget tree ([`ui`]). What it needs from the
//! system it runs on comes in through [`Host`]; the LazyOS `writer` binary
//! (`xui-app/src/bin/writer.rs`) supplies the backend, fonts, atomic writes
//! and the start folder.
//!
//! ```ignore
//! run_app(backend, spec, |ui| xui_writer::ui::build(ui, host).expect("built"))
//! ```

pub mod app;
pub mod commands;
pub mod files;
pub mod host;
pub mod names;
pub mod probe;
pub mod ui;

pub use app::{Msg, Writer};
pub use host::Host;
