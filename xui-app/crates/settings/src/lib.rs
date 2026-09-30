#![forbid(unsafe_code)]

//! The Settings app core.
//!
//! Everything OS-specific sits behind [`ConfigStore`] (confd on LazyOS, a map
//! in tests), so the sections, presets and UI run and are tested on the host.
//! Settings are confd keys: `sys/ui/*` (theme, followed live by `xuid`) and
//! `sys/input/layout` (followed live by `inputd`).

pub mod app;
pub mod keyboard;
pub mod sections;
pub mod store;
pub mod theme_ops;

pub use app::{Msg, SettingsApp};
pub use sections::Section;
pub use store::{ConfigStore, MemStore};
