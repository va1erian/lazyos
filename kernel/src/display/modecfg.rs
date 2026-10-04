//! The `display.*` lines of `lazyos.cfg` (docs/hidpi-plan.md, D1).
//!
//! ```text
//! display.mode=2560x1440   # ask the adapter for this mode after boot
//! display.scale=auto       # console scale: auto, 1 or 2
//! ```
//!
//! The file is untrusted input. [`parse`] is pure so the test suite can
//! feed it hostile text. A bad or repeated line is reported and ignored on
//! its own: like `limit.*`, a display line can never cost the boot its root
//! volume or its working firmware mode.

/// The prefix every display line starts with; `fs::bootcfg` skips these.
pub const PREFIX: &str = "display.";

/// Smallest and largest mode accepted per axis. The upper bound is the Bochs
/// DISPI maximum QEMU's std VGA advertises; the adapter's own limits are
/// checked again before a switch.
pub const MIN_WIDTH: u32 = 640;
pub const MIN_HEIGHT: u32 = 480;
pub const MAX_WIDTH: u32 = 3840;
pub const MAX_HEIGHT: u32 = 2160;

/// The largest scale anything draws at.
pub const MAX_SCALE: u32 = 2;
/// The smallest logical screen a scale may leave: a 720p desktop.
pub const LOGICAL_MIN: (u32, u32) = (1280, 720);

/// How the scale is chosen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scale {
    /// [`auto_scale`] of the screen.
    #[default]
    Auto,
    Fixed(u32),
}

impl Scale {
    /// The scale in effect on a `width x height` screen.
    pub fn resolve(self, width: u32, height: u32) -> u32 {
        match self {
            Scale::Auto => auto_scale(width, height),
            Scale::Fixed(scale) => scale,
        }
    }
}

/// The parsed display settings; absent lines leave the defaults (the
/// firmware mode, automatic scale).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DisplayCfg {
    pub mode: Option<(u32, u32)>,
    pub scale: Scale,
}

/// What became of one `display.*` line, for the boot log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Problem<'a> {
    Unknown(&'a str),
    Malformed(&'a str),
    Duplicate(&'a str),
}

/// The largest integer scale up to [`MAX_SCALE`] that still leaves a logical
/// screen of at least [`LOGICAL_MIN`]: 2560x1440 is 2, 1920x1080 and
/// 1280x720 are 1. The same rule as `uitheme::auto_scale`, which `xuid` uses.
pub fn auto_scale(width: u32, height: u32) -> u32 {
    (1..=MAX_SCALE)
        .rev()
        .find(|scale| width / scale >= LOGICAL_MIN.0 && height / scale >= LOGICAL_MIN.1)
        .unwrap_or(1)
}

/// Parse the `display.*` lines of a config text, calling `report` for every
/// line that is ignored. Other lines are not this module's business.
pub fn parse<'a>(text: &'a str, mut report: impl FnMut(Problem<'a>)) -> DisplayCfg {
    let mut cfg = DisplayCfg::default();
    let (mut seen_mode, mut seen_scale) = (false, false);
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(PREFIX) else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            report(Problem::Malformed(rest.trim()));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        // A trailing `# comment` is allowed, as in the example above.
        let value = value.split('#').next().unwrap_or("").trim();
        let seen = match key {
            "mode" => &mut seen_mode,
            "scale" => &mut seen_scale,
            _ => {
                report(Problem::Unknown(key));
                continue;
            }
        };
        if core::mem::replace(seen, true) {
            report(Problem::Duplicate(key));
            continue;
        }
        let parsed = match key {
            "mode" => parse_mode(value).map(|mode| cfg.mode = Some(mode)),
            _ => parse_scale(value).map(|scale| cfg.scale = scale),
        };
        if parsed.is_none() {
            report(Problem::Malformed(key));
        }
    }
    cfg
}

/// `<width>x<height>`, both inside the accepted range.
pub fn parse_mode(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(['x', 'X'])?;
    let width: u32 = parse_number(width)?;
    let height: u32 = parse_number(height)?;
    let fits =
        (MIN_WIDTH..=MAX_WIDTH).contains(&width) && (MIN_HEIGHT..=MAX_HEIGHT).contains(&height);
    fits.then_some((width, height))
}

/// `auto`, or an integer from 1 to [`MAX_SCALE`].
pub fn parse_scale(value: &str) -> Option<Scale> {
    if value.eq_ignore_ascii_case("auto") {
        return Some(Scale::Auto);
    }
    let scale: u32 = parse_number(value)?;
    (1..=MAX_SCALE)
        .contains(&scale)
        .then_some(Scale::Fixed(scale))
}

/// Decimal digits only (no sign, no spaces inside), at most 5 of them.
fn parse_number(text: &str) -> Option<u32> {
    let ok = !text.is_empty() && text.len() <= 5 && text.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| text.parse().ok()).flatten()
}
