//! The native syscalls for ring-3 programs: `lazyos-sys` (issue #666), the
//! one crate that issues `int 0x80` for native and static-musl programs
//! alike, re-exported flat as the runtime has always named them, plus the
//! runtime's own views ([`args`], [`env`], [`service_args`]) and the FUSE
//! provider ([`fuse`]).

mod args;
pub mod fuse;

pub use args::*;
pub use lazyos_sys::cred::*;
pub use lazyos_sys::display::*;
pub use lazyos_sys::inet::*;
pub use lazyos_sys::input::*;
pub use lazyos_sys::kill::*;
pub use lazyos_sys::msg::{buffer_close, buffer_create, buffer_map};
pub use lazyos_sys::nr;
pub use lazyos_sys::process::*;
pub use lazyos_sys::random::*;
pub use lazyos_sys::spawn::{
    spawnv, spawnv_stdio, Personality, SpawnCred, Stdio, SPAWN_BLOCK_MAX, SPAWN_COUNT_MAX,
    SPAWN_PATH_MAX,
};
pub use lazyos_sys::stats::*;
pub use lazyos_sys::storage::*;
pub use lazyos_sys::time::{
    clock, monotonic_ms, monotonic_ns, nap, sleep_ns, sleep_until_ns, wall_centis, wall_set,
    TICK_NS,
};
