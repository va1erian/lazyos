//! The versioned `FabricStats` snapshot (syscall 5 `stats`), decoded by
//! `lazyos-sys` for the native runtime and this dashboard alike.

pub use lazyos_sys::msg::{fabric_stats, FabricStats, TaskUsage, FABRIC_TASKS};
