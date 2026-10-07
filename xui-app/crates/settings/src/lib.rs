#![forbid(unsafe_code)]

//! The Settings app core.
//!
//! Everything OS-specific sits behind [`ConfigStore`] (confd on LazyOS, a map
//! in tests), so the sections, presets and UI run and are tested on the host.
//! Settings are confd keys: `sys/ui/*` (theme, followed live by `xuid`; the
//! desktop picture `sys/ui/wallpaper`, followed by LazyShell; a user other
//! than the administrator edits its own `user/<uid>/ui/*` copy instead,
//! [`user_theme`]) and
//! `sys/ui/menu` (the desktop context menu, followed live by `xuid`),
//! `user/<uid>/menu/hidden/*` (the apps the start menu leaves out, read by
//! LazyShell each time the menu opens),
//! `sys/time/*` (the taskbar clock format, followed live by `xuid`) and
//! `sys/input/layout` (followed live by `inputd`). The clock, the time zone
//! and the About facts come through [`System`] (`timed`, `sysinfo`); the
//! Accounts page through [`Accounts`] (`accountsd`, and `elevd` for what an
//! administrator must approve).

pub mod about_page;
pub mod accounts;
pub mod accounts_page;
pub mod app;
pub mod appearance_page;
pub mod hidden_ops;
pub mod hidden_page;
pub mod keyboard;
pub mod keyboard_page;
pub mod menu_ops;
pub mod menu_page;
pub mod sections;
pub mod store;
pub mod system;
pub mod theme_ops;
pub mod time_ops;
pub mod time_page;
pub mod user_theme;
pub mod wallpaper_ops;
pub mod windows_page;

pub use accounts::{Account, Accounts, MemAccounts};
pub use app::{Msg, SettingsApp};
pub use sections::Section;
pub use store::{AppChoice, ConfigStore, MemStore};
pub use system::{MemSystem, System};
