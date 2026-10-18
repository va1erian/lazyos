//! The NES Player (`os.lazy.nes`): an NES emulator with sound.
//!
//! The emulation is [`tetanes_core`] (TetaNES, MIT OR Apache-2.0): CPU, PPU,
//! APU, mappers, save states. This crate is the LazyOS app around it — the
//! session ([`session`]), the keyboard ([`input`]), the scaler ([`video`]),
//! the files ([`paths`]), the screen ([`screen`]) and the window ([`app`]).
//! `xui-app`'s `xui-nes` binary supplies the LazyOS platform (the mixer,
//! `$HOME`, the theme and the evidence lines) through [`Host`], so everything
//! here is host-testable.

pub mod app;
pub mod audio;
pub mod commands;
pub mod host;
pub mod input;
pub mod pacer;
pub mod paths;
pub mod rom;
pub mod screen;
pub mod session;
pub mod settings;
pub mod ui;
pub mod video;

pub use app::{Msg, NesApp, WINDOW};
pub use commands::Command;
pub use host::{Host, Report};
pub use rom::RomInfo;
pub use session::Session;
