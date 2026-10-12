//! When the screen goes dark (docs/architecture/display.md, "Screen blanking").
//!
//! `sys/ui/screen/off_minutes` is the number of minutes without keyboard or
//! pointer input after which `xuid` blanks the screen to black; any input
//! wakes it. `0` means never. Like the cursor size the key is not in
//! [`crate::ALL_KEYS`]: it has no palette effect and `xuid` follows it on its
//! own.

use confd::Value;

/// The screen blanking setting's key.
pub const KEY_SCREEN_OFF: &str = "sys/ui/screen/off_minutes";

/// Every screen blanking key (what Settings clears on reset).
pub const SCREEN_KEYS: [&str; 1] = [KEY_SCREEN_OFF];

/// The choices a settings page offers; `0` is "never".
pub const SCREEN_OFF_CHOICES: [u32; 7] = [1, 2, 5, 10, 15, 30, 0];

/// The longest delay accepted (a day); a larger value is read as the default.
pub const MAX_SCREEN_OFF_MINUTES: u32 = 24 * 60;

/// The delay when the key is absent or unreadable. Off: a video, a game or a
/// slide show would otherwise go dark under a hand that has not touched the
/// machine in a while, and no app can yet ask the compositor to stay awake.
pub const DEFAULT_SCREEN_OFF_MINUTES: u32 = 0;

/// The minutes a stored value names (a number, or its text); anything else,
/// including a negative or over-long delay, is the default.
pub fn screen_off_from_value(value: Option<&Value>) -> u32 {
    let minutes = match value {
        Some(Value::U64(minutes)) => Some(*minutes),
        Some(Value::I64(minutes)) => u64::try_from(*minutes).ok(),
        Some(Value::Str(text)) => text.trim().parse::<u64>().ok(),
        _ => None,
    };
    minutes
        .filter(|&minutes| minutes <= u64::from(MAX_SCREEN_OFF_MINUTES))
        .map_or(DEFAULT_SCREEN_OFF_MINUTES, |minutes| minutes as u32)
}

/// The name a settings page shows for a delay: "Never", "1 minute" or
/// "N minutes" (a `Display`, so `no_std` callers need no allocation).
pub struct ScreenOffLabel(pub u32);

impl core::fmt::Display for ScreenOffLabel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            0 => f.write_str("Never"),
            1 => f.write_str("1 minute"),
            n => write!(f, "{n} minutes"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::{String, ToString};

    #[test]
    fn reads_the_setting_tolerantly() {
        assert_eq!(screen_off_from_value(None), DEFAULT_SCREEN_OFF_MINUTES);
        assert_eq!(screen_off_from_value(Some(&Value::U64(5))), 5);
        assert_eq!(screen_off_from_value(Some(&Value::I64(30))), 30);
        assert_eq!(screen_off_from_value(Some(&Value::U64(0))), 0);
        assert_eq!(
            screen_off_from_value(Some(&Value::Str(" 15 ".to_string()))),
            15
        );
        for bad in [
            Value::I64(-1),
            Value::U64(u64::MAX),
            Value::U64(1441),
            Value::Bool(true),
            Value::Str("soon".to_string()),
        ] {
            assert_eq!(
                screen_off_from_value(Some(&bad)),
                DEFAULT_SCREEN_OFF_MINUTES
            );
        }
        assert_eq!(screen_off_from_value(Some(&Value::U64(1440))), 1440);
    }

    #[test]
    fn every_choice_has_a_label() {
        let labels: std::vec::Vec<String> = SCREEN_OFF_CHOICES
            .iter()
            .map(|&minutes| ScreenOffLabel(minutes).to_string())
            .collect();
        assert_eq!(labels[0], "1 minute");
        assert_eq!(labels[3], "10 minutes");
        assert_eq!(labels[6], "Never");
    }
}
