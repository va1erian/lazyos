//! Validation of what a client asks for: the size of a view, its `LightConfig`, the base URL of a
//! page and the strings that go to the engine as NUL-terminated C strings.
//!
//! A light view cannot be widened: `LightConfig` has four fields and the engine's configuration is
//! built from exactly those, with `light` always set; a field an older or hostile client adds is skipped
//! by the generated decoder and never reaches the engine.

use messenger_generated::os_lazy_webd_light_v1 as wire;

use crate::engine::EngineConfig;
use crate::limits::{PIXELS_MAX, SIZE_MAX, USER_STYLE_BYTES_MAX};

/// Why a request was refused: the positive errno and a short friendly text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: i64,
    pub text: &'static str,
}

impl Refusal {
    pub const fn new(code: i64, text: &'static str) -> Self {
        Refusal { code, text }
    }
}

const BAD_SIZE: Refusal = Refusal::new(crate::fabric::errno::EINVAL, "bad view size");

/// A size within `1..=SIZE_MAX` on each side and `PIXELS_MAX` in all.
pub fn check_size(width: u32, height: u32) -> Result<(i32, i32), Refusal> {
    if !(1..=SIZE_MAX).contains(&width) || !(1..=SIZE_MAX).contains(&height) {
        return Err(BAD_SIZE);
    }
    if u64::from(width) * u64::from(height) > PIXELS_MAX {
        return Err(BAD_SIZE);
    }
    // Both are at most 8192, so they fit.
    Ok((width as i32, height as i32))
}

/// Bytes of one whole RGBA frame of the size (already checked).
pub fn frame_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * 4
}

/// The engine's configuration for a light view of this size and group.
pub fn engine_config(
    config: &wire::LightConfig,
    width: u32,
    height: u32,
    group: i32,
) -> Result<EngineConfig, Refusal> {
    let (width, height) = check_size(width, height)?;
    let invalid = |text| Refusal::new(crate::fabric::errno::EINVAL, text);
    let color_scheme = match config.color_scheme {
        wire::COLOR_SCHEME_AUTO => 0,
        wire::COLOR_SCHEME_DARK => 1,
        _ => return Err(invalid("unknown color scheme")),
    };
    let default_font_size =
        i32::try_from(config.default_font_size).map_err(|_| invalid("font size out of range"))?;
    if config.user_style.len() > USER_STYLE_BYTES_MAX {
        return Err(invalid("user style too long"));
    }
    if !c_string_safe(&config.user_style) {
        return Err(invalid("user style holds a NUL"));
    }
    Ok(EngineConfig {
        scripts: config.scripts,
        width,
        height,
        group,
        color_scheme,
        default_font_size,
        user_style: config.user_style.clone(),
    })
}

/// Whether the text can travel as a NUL-terminated C string unchanged.
pub fn c_string_safe(text: &str) -> bool {
    !text.as_bytes().contains(&0)
}

/// The base URL of a page: empty (`about:blank`), any `about:` URL or a `data:` URL. Nothing with a
/// network, `file:` or unknown scheme: a light page is given its content, it never fetches any, and
/// a base that names a place would let relative URLs in the page point there.
pub fn check_base_url(base_url: &str) -> Result<&str, Refusal> {
    let refused = Refusal::new(
        crate::fabric::errno::EINVAL,
        "base URL must be about: or data:",
    );
    if base_url.is_empty() {
        return Ok("about:blank");
    }
    if !c_string_safe(base_url) {
        return Err(refused);
    }
    let scheme_end = base_url.find(':').ok_or(refused)?;
    let scheme = &base_url[..scheme_end];
    if scheme.eq_ignore_ascii_case("about") || scheme.eq_ignore_ascii_case("data") {
        Ok(base_url)
    } else {
        Err(refused)
    }
}

/// `text` cut to at most `max` bytes at a character boundary.
pub fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light() -> wire::LightConfig {
        wire::LightConfig::default()
    }

    #[test]
    fn sizes_are_bounded_per_side_and_in_area() {
        assert_eq!(check_size(1, 1), Ok((1, 1)));
        assert_eq!(check_size(4096, 4096), Ok((4096, 4096)));
        assert_eq!(check_size(8192, 2048), Ok((8192, 2048)));
        for (w, h) in [
            (0, 10),
            (10, 0),
            (8193, 10),
            (10, 8193),
            (4097, 4096),
            (8192, 8192),
            (u32::MAX, 1),
        ] {
            assert!(check_size(w, h).is_err(), "{w}x{h}");
        }
    }

    #[test]
    fn a_default_config_is_a_quiet_light_view() {
        let config = engine_config(&light(), 640, 480, 3).unwrap();
        assert_eq!(
            config,
            EngineConfig {
                scripts: false,
                width: 640,
                height: 480,
                group: 3,
                color_scheme: 0,
                default_font_size: 0,
                user_style: String::new(),
            }
        );
    }

    #[test]
    fn the_four_fields_are_checked() {
        let ok = wire::LightConfig {
            scripts: true,
            color_scheme: wire::COLOR_SCHEME_DARK,
            default_font_size: 18,
            user_style: "p { color: red }".into(),
        };
        let config = engine_config(&ok, 10, 10, 1).unwrap();
        assert!(config.scripts);
        assert_eq!((config.color_scheme, config.default_font_size), (1, 18));
        let bad_scheme = wire::LightConfig {
            color_scheme: 2,
            ..light()
        };
        assert!(engine_config(&bad_scheme, 10, 10, 1).is_err());
        let bad_font = wire::LightConfig {
            default_font_size: u32::MAX,
            ..light()
        };
        assert!(engine_config(&bad_font, 10, 10, 1).is_err());
        let big = wire::LightConfig {
            user_style: "a".repeat(USER_STYLE_BYTES_MAX),
            ..light()
        };
        assert!(engine_config(&big, 10, 10, 1).is_ok());
        let bigger = wire::LightConfig {
            user_style: "a".repeat(USER_STYLE_BYTES_MAX + 1),
            ..light()
        };
        assert!(engine_config(&bigger, 10, 10, 1).is_err());
        let nul = wire::LightConfig {
            user_style: "a\0b".into(),
            ..light()
        };
        assert!(engine_config(&nul, 10, 10, 1).is_err());
    }

    #[test]
    fn only_about_and_data_bases_are_accepted() {
        assert_eq!(check_base_url(""), Ok("about:blank"));
        assert_eq!(check_base_url("about:blank"), Ok("about:blank"));
        assert_eq!(check_base_url("ABOUT:srcdoc"), Ok("ABOUT:srcdoc"));
        assert_eq!(check_base_url("data:text/html,hi"), Ok("data:text/html,hi"));
        for bad in [
            "http://example.com/",
            "https://example.com/",
            "file:///etc/passwd",
            "ftp://x/",
            "blob:https://x/1",
            "javascript:alert(1)",
            "example.com",
            "/relative",
            "about:blank\0http://x/",
        ] {
            assert!(check_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn clip_cuts_at_a_character_boundary() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abc", 2), "ab");
        assert_eq!(clip("aé", 2), "a");
        assert_eq!(clip("日本", 4), "日");
        assert_eq!(clip("日本", 0), "");
    }
}
