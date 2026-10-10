//! Kernel limits for `lazyos.cfg`: each `LAZYOS_LIMIT_<KEY>=<value>` build
//! variable becomes a `limit.<key>=<value>` line the kernel reads at boot
//! (`kernel/src/limits.rs`, docs/architecture/limits.md).
//!
//! The kernel treats the file as untrusted and clamps every value into its
//! own range, so the build only checks the syntax: a typo fails the build
//! here instead of being ignored with a log line at boot.

/// The configurable limits, as the kernel names them (`limits::KEYS`).
pub const KEYS: [&str; 7] = [
    "heap_max",
    "fd_max",
    "stack_size",
    "quota_user_memory",
    "quota_kernel_memory",
    "shared_buffer_max",
    "scratch_max",
];

/// Keys whose value is a count rather than a byte size (no `K`/`M`/`G`/`T`).
const COUNTS: [&str; 1] = ["fd_max"];

/// The environment variable that sets `key`.
pub fn env_name(key: &str) -> String {
    format!("LAZYOS_LIMIT_{}", key.to_ascii_uppercase())
}

/// Whether `value` is a value the kernel accepts for `key`: decimal digits,
/// plus one binary-multiple suffix for a byte size.
pub fn validate(key: &str, value: &str) -> Result<(), String> {
    let digits = match value.as_bytes().last() {
        Some(b'k' | b'K' | b'm' | b'M' | b'g' | b'G' | b't' | b'T') if !COUNTS.contains(&key) => {
            &value[..value.len() - 1]
        }
        _ => value,
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "{}={value:?}: expected digits{}",
            env_name(key),
            if COUNTS.contains(&key) {
                ""
            } else {
                " with an optional K, M, G or T suffix"
            }
        ));
    }
    if digits.parse::<u64>().is_err() {
        return Err(format!("{}={value:?}: number too large", env_name(key)));
    }
    Ok(())
}

/// The `(key, value)` pairs set in the environment, in [`KEYS`] order.
/// Registers every variable with cargo, so changing one rebuilds the image.
/// Panics (failing the build) on a malformed value.
pub fn from_env() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for key in KEYS {
        let name = env_name(key);
        println!("cargo:rerun-if-env-changed={name}");
        if let Ok(value) = std::env::var(&name) {
            let value = value.trim().to_string();
            if let Err(error) = validate(key, &value) {
                panic!("{error}");
            }
            out.push((key.to_string(), value));
        }
    }
    out
}

/// The `lazyos.cfg` lines for `limits`.
pub fn lines(limits: &[(String, String)]) -> String {
    limits
        .iter()
        .map(|(key, value)| format!("limit.{key}={value}\n"))
        .collect()
}
