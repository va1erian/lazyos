//! Engine resource limits, on by default (issue #319).
//!
//! Limits are a second, finer layer than the process sandbox: they stop a
//! runaway or hostile script from spinning forever, recursing until the small
//! ring-3 stack overflows, or building unbounded strings/arrays/maps.

/// Every knob the host exposes as a flag. `0` disables a limit where noted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Operations before the script is stopped (`0` = unlimited).
    pub max_operations: u64,
    /// Nested script-function calls. The guest main thread has a 1 MiB stack,
    /// so this stays well below Rhai's release default of 64.
    pub max_call_levels: usize,
    /// Expression nesting at the top level and inside functions.
    pub max_expr_depth: usize,
    pub max_fn_expr_depth: usize,
    /// Bytes in one string (`0` = unlimited).
    pub max_string_size: usize,
    /// Elements in one array / map (`0` = unlimited).
    pub max_array_size: usize,
    pub max_map_size: usize,
    /// Bytes `os::read`, `stdin_text` and script files may load. Never
    /// unlimited: reads are capped as they happen. Kept equal to the string
    /// limit by default, since a text read longer than that could not become a
    /// Rhai string anyway.
    pub max_io_bytes: usize,
    /// Longest single `sleep(ms)`. Sleeping is not counted as operations, so
    /// without a cap one call could park the process for good.
    pub max_sleep_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_operations: 10_000_000,
            max_call_levels: 32,
            max_expr_depth: 64,
            max_fn_expr_depth: 32,
            max_string_size: 4 << 20,
            max_array_size: 100_000,
            max_map_size: 100_000,
            max_io_bytes: 4 << 20,
            max_sleep_ms: 3_600_000,
        }
    }
}
