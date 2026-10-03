//! The desktop picture setting (`sys/ui/wallpaper`), read by LazyShell and
//! written by the Settings app.

use confd::Value;

/// The desktop picture LazyShell draws over the background colour: the
/// absolute path of a PNG or JPEG file (see [`wallpaper_path`]). A string, so
/// it is not part of [`Settings`](crate::Settings), which `xuid` copies
/// around; the compositor never reads it.
pub const KEY_WALLPAPER: &str = "sys/ui/wallpaper";

/// The picture path a [`KEY_WALLPAPER`] entry names: `None` (the plain
/// background colour) for a missing key, a value of the wrong kind, an empty
/// string or a relative path.
pub fn wallpaper_path(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::Str(path)) if path.starts_with('/') => Some(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallpaper_path_takes_only_an_absolute_path() {
        let text = |s: &str| Value::Str(s.into());
        let picture = text("/system/share/wallpapers/Aurora.jpg");
        assert_eq!(
            wallpaper_path(Some(&picture)),
            Some("/system/share/wallpapers/Aurora.jpg")
        );
        assert_eq!(wallpaper_path(None), None);
        assert_eq!(wallpaper_path(Some(&text(""))), None);
        assert_eq!(wallpaper_path(Some(&text("Aurora.jpg"))), None);
        assert_eq!(wallpaper_path(Some(&Value::U64(7))), None);
        assert!(KEY_WALLPAPER.starts_with(crate::PREFIX));
        assert!(!crate::ALL_KEYS.contains(&KEY_WALLPAPER));
    }
}
