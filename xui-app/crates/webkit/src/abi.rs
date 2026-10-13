//! The parts of the C ABI (`ffi.rs`) that are pure: how text crosses it, which argument is the
//! first address, how big a frame is, how the page's state arrives. They have no pointer and no
//! musl-only dependency, so the host tests run them.

use std::ffi::CString;

use crate::address;
use crate::mailbox::PageState;

/// A C string from text that may not be one (a NUL ends it).
pub fn c_string(text: &str) -> CString {
    let end = text.find('\0').unwrap_or(text.len());
    CString::new(&text[..end]).unwrap_or_default()
}

/// The address to open first: the first argument after the program's name that is neither an
/// option (`-...`) nor the supervisor's `attempt=` note, made into a URL.
pub fn first_address(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1)
        .find(|arg| !arg.starts_with('-') && !arg.starts_with("attempt="))
        .and_then(|arg| address::normalize(arg))
}

/// A frame's size and byte length (`width * height * 4`): `None` for a negative or empty size,
/// or one whose length does not fit.
pub fn frame_geometry(width: i32, height: i32) -> Option<(u32, u32, usize)> {
    let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    let len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    (len != 0).then_some((width, height, len))
}

/// The page's state as the engine sent it, with the progress clamped to 0..=1000.
#[allow(clippy::too_many_arguments)]
pub fn page_state(
    url: String,
    title: String,
    loading: i32,
    can_back: i32,
    can_forward: i32,
    progress: i32,
    error: String,
    crashed: i32,
) -> PageState {
    PageState {
        url,
        title,
        loading: loading != 0,
        can_back: can_back != 0,
        can_forward: can_forward != 0,
        progress: u32::try_from(progress).unwrap_or(0).min(1000),
        error,
        crashed: crashed != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn text_ends_at_its_first_nul() {
        assert_eq!(c_string("abc").to_bytes(), b"abc");
        assert_eq!(c_string("ab\0cd").to_bytes(), b"ab");
        assert_eq!(c_string("").to_bytes(), b"");
        assert_eq!(c_string("\0x").to_bytes(), b"");
    }

    #[test]
    fn the_first_address_skips_the_program_options_and_the_supervisors_note() {
        assert_eq!(first_address(&args(&["webkit-app"])), None);
        assert_eq!(
            first_address(&args(&["webkit-app", "example.com"])),
            Some("http://example.com/".to_owned())
        );
        assert_eq!(
            first_address(&args(&["webkit-app", "--client", "attempt=1", "http://a/"])),
            Some("http://a/".to_owned())
        );
        assert_eq!(first_address(&args(&["webkit-app", "attempt=2"])), None);
        // The program's own name is never an address.
        assert_eq!(first_address(&args(&["http://a/"])), None);
    }

    #[test]
    fn a_frame_is_width_times_height_times_four_bytes() {
        assert_eq!(frame_geometry(3, 2), Some((3, 2, 24)));
        assert_eq!(frame_geometry(0, 5), None);
        assert_eq!(frame_geometry(5, 0), None);
        assert_eq!(frame_geometry(-1, 5), None);
        assert_eq!(frame_geometry(5, -1), None);
        // i32::MAX squared times four fits a 64-bit usize and overflows a 32-bit one.
        assert_eq!(
            frame_geometry(i32::MAX, i32::MAX).is_some(),
            usize::BITS == 64
        );
    }

    #[test]
    fn the_state_clamps_progress_and_reads_flags_as_booleans() {
        let state = page_state("u".into(), "t".into(), 2, 0, -1, 4000, "e".into(), 1);
        assert!(state.loading && !state.can_back && state.can_forward && state.crashed);
        assert_eq!(state.progress, 1000);
        assert_eq!((state.url.as_str(), state.title.as_str(), state.error.as_str()), ("u", "t", "e"));
        assert_eq!(page_state(String::new(), String::new(), 0, 0, 0, -5, String::new(), 0).progress, 0);
    }
}
