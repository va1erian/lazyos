//! The desktop's integer UI scale (docs/hidpi-plan.md).
//!
//! `sys/ui/scale` is `auto` (also the meaning of an absent key), `1` or `2`.
//! `xuid` resolves it once at start against the screen, draws its chrome at
//! that scale and hands it to every client (`GetOutput`), so an xui app runs
//! at `96 * scale` DPI. The same rule picks the kernel console's scale
//! (`kernel/src/display/modecfg.rs`).
//!
//! The image's default is the `display.scale=` line of `/boot/lazyos.cfg`
//! (`LAZYOS_DISPLAY_SCALE` at build time): [`UiScale::from_cfg`], which `xuid`
//! uses when `sys/ui/scale` is not set. `LAZYOS_DISPLAY_SCALE=1` on a
//! 2560x1440 screen is the roomy "hi-res" desktop (no doubling).

use confd::Value;

/// The scale setting's key.
pub const KEY_SCALE: &str = "sys/ui/scale";

/// The largest scale anything draws at.
pub const MAX_SCALE: u32 = 2;
/// The smallest logical screen a scale may leave: a 720p desktop.
pub const LOGICAL_MIN: (u32, u32) = (1280, 720);
/// The DPI an xui app runs at for scale 1.
pub const BASE_DPI: u32 = 96;

/// How the scale is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiScale {
    /// [`auto_scale`] of the screen.
    #[default]
    Auto,
    Fixed(u32),
}

impl UiScale {
    /// `auto`, `1` or `2`; anything else is `None` (the caller keeps auto).
    pub fn parse(text: &str) -> Option<UiScale> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("auto") {
            return Some(UiScale::Auto);
        }
        match text.parse::<u32>() {
            Ok(scale) if (1..=MAX_SCALE).contains(&scale) => Some(UiScale::Fixed(scale)),
            _ => None,
        }
    }

    /// The setting from a confd value: a string as above, or an integer.
    pub fn from_value(value: Option<&Value>) -> UiScale {
        match value {
            Some(Value::Str(text)) => UiScale::parse(text).unwrap_or_default(),
            Some(Value::I64(scale)) => UiScale::from_int(i128::from(*scale)),
            Some(Value::U64(scale)) => UiScale::from_int(i128::from(*scale)),
            _ => UiScale::Auto,
        }
    }

    fn from_int(scale: i128) -> UiScale {
        match u32::try_from(scale) {
            Ok(scale) if (1..=MAX_SCALE).contains(&scale) => UiScale::Fixed(scale),
            _ => UiScale::Auto,
        }
    }

    /// The `display.scale=` line of a `lazyos.cfg` text, read as the
    /// kernel reads it (`kernel/src/display/modecfg.rs::parse`, which this
    /// mirrors line for line): a line is `display.<key>=<value>` with the key
    /// compared exactly (so `display.scales=1` is another key), a value may
    /// end in a `# comment`, and the FIRST `display.scale=` line decides even
    /// if its value is bad (the kernel ignores later duplicates, so a later
    /// valid line must not win here). A bare `display.scale` has no `=`: the
    /// kernel skips it as malformed without marking the key seen. `None`
    /// when there is no deciding line or its value is refused.
    pub fn from_cfg(cfg: &str) -> Option<UiScale> {
        for line in cfg.lines() {
            let Some(rest) = line.trim().strip_prefix("display.") else {
                continue;
            };
            let Some((key, value)) = rest.split_once('=') else {
                continue;
            };
            if key.trim() != "scale" {
                continue;
            }
            let value = value.trim().split('#').next().unwrap_or("").trim();
            return cfg_value(value);
        }
        None
    }

    /// The scale in effect on a `width x height` screen.
    pub fn resolve(self, width: u32, height: u32) -> u32 {
        match self {
            UiScale::Auto => auto_scale(width, height),
            UiScale::Fixed(scale) => scale,
        }
    }

    /// The text form written to confd.
    pub const fn as_str(self) -> &'static str {
        match self {
            UiScale::Auto => "auto",
            UiScale::Fixed(1) => "1",
            UiScale::Fixed(_) => "2",
        }
    }
}

/// A `display.scale` value as the kernel's `parse_scale` takes it: `auto`,
/// or decimal digits only (no sign) that make 1 to [`MAX_SCALE`].
fn cfg_value(value: &str) -> Option<UiScale> {
    if value.eq_ignore_ascii_case("auto") {
        return Some(UiScale::Auto);
    }
    let digits = !value.is_empty() && value.len() <= 5 && value.bytes().all(|b| b.is_ascii_digit());
    let scale: u32 = value.parse().ok().filter(|_| digits)?;
    (1..=MAX_SCALE)
        .contains(&scale)
        .then_some(UiScale::Fixed(scale))
}

/// The largest integer scale up to [`MAX_SCALE`] that still leaves a logical
/// screen of at least [`LOGICAL_MIN`]: 2560x1440 is 2, 1920x1080 is 1.
pub fn auto_scale(width: u32, height: u32) -> u32 {
    (1..=MAX_SCALE)
        .rev()
        .find(|scale| width / scale >= LOGICAL_MIN.0 && height / scale >= LOGICAL_MIN.1)
        .unwrap_or(1)
}

/// The DPI an xui app runs at on a desktop of `scale`.
pub fn dpi_for(scale: u32) -> u32 {
    BASE_DPI * scale.clamp(1, MAX_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_keeps_a_720p_logical_screen() {
        assert_eq!(auto_scale(2560, 1440), 2);
        assert_eq!(auto_scale(3840, 2160), 2);
        assert_eq!(auto_scale(1920, 1080), 1);
        assert_eq!(auto_scale(1280, 720), 1);
        assert_eq!(auto_scale(2559, 1440), 1);
        assert_eq!(auto_scale(0, 0), 1);
    }

    #[test]
    fn reads_the_boot_cfg_default() {
        let cfg = "root=UUID=x\ndisplay.mode=2560x1440\ndisplay.scale = 1  # roomy\n";
        assert_eq!(UiScale::from_cfg(cfg), Some(UiScale::Fixed(1)));
        assert_eq!(
            UiScale::from_cfg("display.scale=2\n"),
            Some(UiScale::Fixed(2))
        );
        assert_eq!(
            UiScale::from_cfg("  display.scale=AUTO"),
            Some(UiScale::Auto)
        );
        // No line, other display keys, or a value the kernel refuses: no default.
        for none in [
            "",
            "display.mode=2560x1440\n",
            "display.scale=3\n",
            "display.scale\n",
            "display.scale=\n",
            "display.scales=1\n",
            "# display.scale=1\n",
            "display.scale=+2\n",
            "display.scale=1.5\n",
        ] {
            assert_eq!(UiScale::from_cfg(none), None, "{none:?}");
        }
        assert_eq!(
            UiScale::from_cfg("display.mode=2560x1440\ndisplay.scale=1\n")
                .map(|scale| scale.resolve(2560, 1440)),
            Some(1)
        );
    }

    /// The kernel's rule, case for case (its twin is
    /// `kernel/src/display/modecfg.rs`'s tests): the key is compared exactly,
    /// a bare `display.scale` is skipped, and the first `display.scale=` line
    /// decides even when its value is bad.
    #[test]
    fn cfg_agrees_with_the_kernel_parser() {
        let cases: [(&str, Option<UiScale>); 8] = [
            (
                "display.scale=1\ndisplay.scale=2\n",
                Some(UiScale::Fixed(1)),
            ),
            (
                "display.scales=1\ndisplay.scale=2\n",
                Some(UiScale::Fixed(2)),
            ),
            ("display.scale\ndisplay.scale=2\n", Some(UiScale::Fixed(2))),
            ("display.scale=3\ndisplay.scale=2\n", None),
            ("display.scale=\ndisplay.scale=2\n", None),
            ("display.scale=auto\ndisplay.scale=2\n", Some(UiScale::Auto)),
            ("display.scale=2 # x=1\n", Some(UiScale::Fixed(2))),
            (
                "display.scale # x=1\ndisplay.scale=1\n",
                Some(UiScale::Fixed(1)),
            ),
        ];
        for (cfg, want) in cases {
            assert_eq!(UiScale::from_cfg(cfg), want, "{cfg:?}");
        }
    }

    #[test]
    fn parses_the_setting() {
        assert_eq!(UiScale::parse("auto"), Some(UiScale::Auto));
        assert_eq!(UiScale::parse(" 2 "), Some(UiScale::Fixed(2)));
        assert_eq!(UiScale::parse("1"), Some(UiScale::Fixed(1)));
        for bad in ["0", "3", "1.5", "", "-1", "two"] {
            assert_eq!(UiScale::parse(bad), None, "{bad}");
        }
        assert_eq!(UiScale::from_value(None), UiScale::Auto);
        assert_eq!(UiScale::from_value(Some(&Value::I64(2))), UiScale::Fixed(2));
        assert_eq!(UiScale::from_value(Some(&Value::I64(-2))), UiScale::Auto);
        assert_eq!(UiScale::from_value(Some(&Value::I64(7))), UiScale::Auto);
        assert_eq!(UiScale::from_value(Some(&Value::U64(1))), UiScale::Fixed(1));
        assert_eq!(UiScale::Fixed(1).resolve(2560, 1440), 1);
        assert_eq!(UiScale::Auto.resolve(2560, 1440), 2);
        assert_eq!(dpi_for(2), 192);
        assert_eq!(dpi_for(9), 192);
        for scale in [UiScale::Auto, UiScale::Fixed(1), UiScale::Fixed(2)] {
            assert_eq!(UiScale::parse(scale.as_str()), Some(scale));
        }
    }
}
