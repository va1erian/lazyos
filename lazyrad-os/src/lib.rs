//! LazyRAD on LazyOS (docs/lazyrad-plan.md).
//!
//! Two programs share this crate: `lrplay`, the player that runs a LazyRAD
//! project (and is the stub inside every produced `.lzp`), and `lazyrad`, the
//! IDE. Both are `xuid` clients built on the portable LazyRAD crates; this
//! library is the LazyOS glue:
//!
//! * [`args`]: the command line `init`'s launcher and a package manifest hand
//!   over, parsed without trusting it;
//! * [`platform`]: the [`lazyrad_runtime::platform::Platform`] LazyOS installs
//!   (config directory, script file sandbox, where the player lives);
//! * [`launcher`]: starting the player from the IDE with pipes polled on the UI
//!   thread (LazyOS threads cannot share descriptors);
//! * [`devplay`]: Play under the project's own permissions when the IDE is a
//!   package (`dev:<system_name>`, issue #529);
//! * [`handoff`]: File → Make LazyOS App: an in-process pre-check, then the
//!   Package Installer through `mimed` (the IDE never calls `pkgd`);
//! * [`transport`]: the Messenger call seam [`handoff`] runs over;
//! * [`migrate`]: the one-time move of `.apps/lazyrad` to the package's
//!   data folder;
//! * [`marker`]: the `LRPLAY:*` / `LRIDE:*` serial evidence lines the
//!   screenshot sessions grep for;
//! * [`messenger`]: the `msg` module for form scripts (Messenger calls,
//!   topics, services), registered as a LazyRAD script extension;
//! * [`tracker`]: the `modplay` module for form scripts (ProTracker songs
//!   played through the system mixer), registered the same way;
//! * [`probe`]: the startup form's control rectangles for session scripts
//!   (`LAZYOS_UI_PROBE=1` images, issue #538).

pub mod args;
pub mod desktop_mode;
#[cfg(unix)]
pub mod devplay;
pub mod failure;
pub mod handoff;
pub mod launcher;
pub mod marker;
pub mod messenger;
pub mod migrate;
pub mod platform;
pub mod playdev;
pub mod probe;
pub mod tracker;
pub mod transport;
