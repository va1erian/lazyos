//! The mouse cursor's size (docs/cursor-plan.md).
//!
//! `sys/ui/cursor/size` is the cursor's design size in pixels at scale 1:
//! one of [`CURSOR_SIZES`]. `xuid` draws it at `size * scale` physical pixels
//! (a real vector rendering, never doubled blocks). The key is not in
//! [`crate::ALL_KEYS`]: it does not move the palette, `xuid` handles it on its
//! own and re-stamps the cursor without a repaint.

use alloc::string::String;
use confd::Value;

/// The cursor size setting's key.
pub const KEY_CURSOR_SIZE: &str = "sys/ui/cursor/size";

/// The cursor theme's key: a `.lcur` theme as text (docs/cursor-editor-plan.md),
/// at most `confd`'s 4 KiB value; empty or absent for the built-in shapes.
pub const KEY_CURSOR_THEME: &str = "sys/ui/cursor/theme";

/// Every cursor setting key.
pub const CURSOR_KEYS: [&str; 2] = [KEY_CURSOR_SIZE, KEY_CURSOR_THEME];

/// The selectable sizes: Small, Medium, Large, Extra large.
pub const CURSOR_SIZES: [u32; 4] = [16, 24, 32, 48];

/// The size when the key is absent or unreadable.
pub const DEFAULT_CURSOR_SIZE: u32 = CURSOR_SIZES[0];

/// The name Settings shows for `size`.
pub fn cursor_size_label(size: u32) -> &'static str {
    match size {
        16 => "Small",
        24 => "Medium",
        32 => "Large",
        48 => "Extra large",
        _ => "Custom",
    }
}

/// `size` when it is one of [`CURSOR_SIZES`], else `None`.
pub fn valid_cursor_size(size: u64) -> Option<u32> {
    CURSOR_SIZES.iter().copied().find(|&s| u64::from(s) == size)
}

/// The size a stored value names: a number, or its text; anything else is
/// the default.
pub fn cursor_size_from_value(value: Option<&Value>) -> u32 {
    let size = match value {
        Some(Value::U64(size)) => Some(*size),
        Some(Value::I64(size)) => u64::try_from(*size).ok(),
        Some(Value::Str(text)) => text.trim().parse::<u64>().ok(),
        _ => None,
    };
    size.and_then(valid_cursor_size)
        .unwrap_or(DEFAULT_CURSOR_SIZE)
}

/// The theme text a stored value holds (`.lcur`, parsed by `cursorgfx`); a
/// theme lives in the key itself, so no file path is ever read. Empty or
/// absent means the built-in shapes.
pub fn cursor_theme_from_value(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::Str(text)) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn reads_the_setting_tolerantly() {
        assert_eq!(cursor_size_from_value(None), 16);
        assert_eq!(cursor_size_from_value(Some(&Value::U64(32))), 32);
        assert_eq!(cursor_size_from_value(Some(&Value::I64(48))), 48);
        assert_eq!(
            cursor_size_from_value(Some(&Value::Str(" 24 ".to_string()))),
            24
        );
        for bad in [Value::U64(17), Value::I64(-1), Value::Bool(true)] {
            assert_eq!(cursor_size_from_value(Some(&bad)), 16);
        }
        assert_eq!(
            cursor_size_from_value(Some(&Value::Str("big".to_string()))),
            16
        );
    }

    #[test]
    fn every_size_has_a_label() {
        for size in CURSOR_SIZES {
            assert_ne!(cursor_size_label(size), "Custom");
        }
    }

    #[test]
    fn the_theme_is_the_stored_text() {
        let text = || {
            Some(Value::Str(
                "lazyos-cursor 1
name a
"
                .to_string(),
            ))
        };
        assert_eq!(
            cursor_theme_from_value(text().as_ref()).as_deref(),
            Some(
                "lazyos-cursor 1
name a
"
            )
        );
        for none in [
            None,
            Some(Value::Str("  ".to_string())),
            Some(Value::U64(3)),
        ] {
            assert_eq!(cursor_theme_from_value(none.as_ref()), None);
        }
    }
}
