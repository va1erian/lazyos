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
//! * [`pkgd`]: the `pkgd` client behind File → Make LazyOS App;
//! * [`marker`]: the `LRPLAY:*` / `LRIDE:*` serial evidence lines the
//!   screenshot sessions grep for;
//! * [`messenger`]: the `msg` module for form scripts (Messenger calls,
//!   topics, services), registered as a LazyRAD script extension;
//! * [`tracker`]: the `modplay` module for form scripts (ProTracker songs
//!   played through the system mixer), registered the same way.

pub mod args;
pub mod launcher;
pub mod marker;
pub mod messenger;
pub mod pkgd;
pub mod platform;
pub mod tracker;
