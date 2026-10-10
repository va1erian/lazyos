//! Display settings for `lazyos.cfg` (docs/hidpi-plan.md, D1):
//! `LAZYOS_DISPLAY_MODE=<W>x<H>` becomes `display.mode=<W>x<H>` and
//! `LAZYOS_DISPLAY_SCALE=auto|1|2` becomes `display.scale=...` and
//! `LAZYOS_DISPLAY_MAX=<W>x<H>` becomes `display.max=<W>x<H>` (the logical
//! screen cap for a firmware framebuffer, issue #717), read by the
//! kernel's `display::modecfg` at boot.
//!
//! The kernel re-checks both against the adapter, so the build checks only
//! the syntax and the kernel's accepted ranges: a typo fails the build here
//! instead of being ignored with a log line at boot.

/// The mode range the kernel accepts (`display::modecfg`).
const MODE_MIN: (u32, u32) = (640, 480);
const MODE_MAX: (u32, u32) = (3840, 2160);

/// Whether `value` is a `<W>x<H>` mode inside the kernel's range.
pub fn validate_mode(value: &str) -> Result<(), String> {
    validate_size("LAZYOS_DISPLAY_MODE", value)
}

/// Whether `value` is a `<W>x<H>` logical screen cap inside the kernel's range.
pub fn validate_max(value: &str) -> Result<(), String> {
    validate_size("LAZYOS_DISPLAY_MAX", value)
}

fn validate_size(name: &str, value: &str) -> Result<(), String> {
    let parsed = value.split_once(['x', 'X']).and_then(|(w, h)| {
        let digits = |text: &str| {
            (!text.is_empty() && text.len() <= 5 && text.bytes().all(|b| b.is_ascii_digit()))
                .then(|| text.parse::<u32>().ok())
                .flatten()
        };
        Some((digits(w)?, digits(h)?))
    });
    match parsed {
        Some((w, h))
            if (MODE_MIN.0..=MODE_MAX.0).contains(&w) && (MODE_MIN.1..=MODE_MAX.1).contains(&h) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "{name}={value:?}: expected <width>x<height> between {}x{} and {}x{}",
            MODE_MIN.0, MODE_MIN.1, MODE_MAX.0, MODE_MAX.1
        )),
    }
}

/// Whether `value` is a scale the kernel accepts.
pub fn validate_scale(value: &str) -> Result<(), String> {
    match value {
        "auto" | "1" | "2" => Ok(()),
        _ => Err(format!(
            "LAZYOS_DISPLAY_SCALE={value:?}: expected auto, 1 or 2"
        )),
    }
}

/// The `lazyos.cfg` lines for a mode, a scale and a logical cap (each may be
/// absent).
pub fn lines(mode: Option<&str>, scale: Option<&str>, max: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(mode) = mode {
        out.push_str(&format!("display.mode={mode}\n"));
    }
    if let Some(scale) = scale {
        out.push_str(&format!("display.scale={scale}\n"));
    }
    if let Some(max) = max {
        out.push_str(&format!(
            "display.max={max}
"
        ));
    }
    out
}

/// The display lines from the environment. Registers all three variables with
/// cargo, so changing one rebuilds the image. Panics (failing the build) on
/// a malformed value.
pub fn from_env() -> String {
    let read = |name: &str| {
        println!("cargo:rerun-if-env-changed={name}");
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let mode = read("LAZYOS_DISPLAY_MODE");
    let scale = read("LAZYOS_DISPLAY_SCALE");
    let max = read("LAZYOS_DISPLAY_MAX");
    if let Some(mode) = &mode {
        validate_mode(mode).unwrap_or_else(|error| panic!("{error}"));
    }
    if let Some(scale) = &scale {
        validate_scale(scale).unwrap_or_else(|error| panic!("{error}"));
    }
    if let Some(max) = &max {
        validate_max(max).unwrap_or_else(|error| panic!("{error}"));
    }
    lines(mode.as_deref(), scale.as_deref(), max.as_deref())
}
