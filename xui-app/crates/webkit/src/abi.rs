//! The parts of the C ABI (`ffi.rs`) that are pure: how text crosses it, which argument is the
//! first address, how big a frame is, how the page's state, a navigation, a download and a dialog
//! arrive. They have no pointer and no musl-only dependency, so the host tests run them.

use std::ffi::CString;

use crate::address;
use crate::events::Event;
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
    committed_url: String,
    secure: i32,
    finished: i32,
) -> PageState {
    PageState {
        url,
        committed_url,
        secure: secure != 0,
        title,
        loading: loading != 0,
        can_back: can_back != 0,
        can_forward: can_forward != 0,
        progress: u32::try_from(progress).unwrap_or(0).min(1000),
        error,
        crashed: crashed != 0,
        finished: u32::try_from(finished).unwrap_or(0),
    }
}

/// What a window asks of the UI process beyond painting the page (`lzw_ui_features`): the
/// UI process installs the client for each bit that is set and nothing for the others, so the
/// window that asks for none behaves as it always did.
pub mod features {
    /// The page's UI client: the link under the pointer (`lzw_ui_hover`), the pointer shape
    /// (`lzw_ui_cursor`) and requests for a new window (`lzw_ui_new_window`).
    pub const PAGE_UI: i32 = 1;
    /// JavaScript `alert`, `confirm` and `prompt` (`lzw_ui_dialog`).
    pub const DIALOGS: i32 = 2;
    /// Responses the page cannot show, and attachments, become downloads
    /// (`lzw_ui_download_start` and the calls after it).
    pub const DOWNLOADS: i32 = 4;
    /// Every navigation asks `lzw_ui_navigate`.
    pub const POLICY: i32 = 8;
    /// The window prints the markers that name pages (`LOAD`, `TITLE`, ...): the UI process
    /// does not.
    pub const OWN_MARKERS: i32 = 16;
    /// A website data store in the profile directory (`lzw_ui_profile_dir`).
    pub const PROFILE: i32 = 32;
    /// The Notifications API: WebKit's notification provider and permission requests
    /// (`lzw_ui_notify_*` and `lzw_ui_notify_permission_at`).
    pub const NOTIFICATIONS: i32 = 64;
    /// Everything LazyWebPlus asks for.
    pub const ALL: i32 =
        PAGE_UI | DIALOGS | DOWNLOADS | POLICY | OWN_MARKERS | PROFILE | NOTIFICATIONS;
}

/// How a navigation began (`WKFrameNavigationType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKind {
    LinkClicked,
    FormSubmitted,
    BackForward,
    Reload,
    FormResubmitted,
    /// A load the UI process started (an address typed), a script's, a redirect.
    Other,
}

impl NavKind {
    /// The ABI value, WebKit's own numbering; anything unknown is `Other`, the kind that earns no
    /// trust.
    pub const fn from_code(code: i32) -> NavKind {
        match code {
            0 => NavKind::LinkClicked,
            1 => NavKind::FormSubmitted,
            2 => NavKind::BackForward,
            3 => NavKind::Reload,
            4 => NavKind::FormResubmitted,
            _ => NavKind::Other,
        }
    }
}

/// What the window decided about a navigation (`lzw_ui_navigate`'s return value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The engine loads it.
    Allow,
    /// Nothing happens.
    Deny,
    /// The window passes it to another app; the engine does not load it.
    Handoff,
}

impl Verdict {
    /// The ABI value: 0 allow, 1 deny, 2 handoff.
    pub const fn code(self) -> i32 {
        match self {
            Verdict::Allow => 0,
            Verdict::Deny => 1,
            Verdict::Handoff => 2,
        }
    }
}

/// A download's size as the engine reports it: a negative number is "not announced".
pub fn byte_count(bytes: i64) -> Option<u64> {
    u64::try_from(bytes).ok()
}

/// What `lzw_ui_notify_show` reports: a page asked to show notification `id`.
pub fn notify_show(
    id: u64,
    origin: String,
    page_url: String,
    title: String,
    body: String,
    tag: String,
) -> Event {
    Event::NotifyShow {
        id,
        origin,
        page_url,
        title,
        body,
        tag,
    }
}

/// What `lzw_ui_notify_permission_request` reports: any non-zero `user_gesture` is a gesture.
pub fn notify_permission(origin: String, user_gesture: i32) -> Event {
    Event::NotifyPermission {
        origin,
        user_gesture: user_gesture != 0,
    }
}

/// Copies `text` and a NUL into `out`; false (and `out` untouched) when they do not fit.
pub fn copy_c_string(text: &str, out: &mut [u8]) -> bool {
    let bytes = text.as_bytes();
    if bytes.contains(&0) || bytes.len() >= out.len() {
        return false;
    }
    out[..bytes.len()].copy_from_slice(bytes);
    out[bytes.len()] = 0;
    true
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
        let state = page_state(
            "u".into(),
            "t".into(),
            2,
            0,
            -1,
            4000,
            "e".into(),
            1,
            "c".into(),
            7,
            3,
        );
        assert!(state.loading && !state.can_back && state.can_forward && state.crashed);
        assert!(state.secure);
        assert_eq!(state.progress, 1000);
        assert_eq!(
            (
                state.url.as_str(),
                state.title.as_str(),
                state.error.as_str(),
                state.committed_url.as_str()
            ),
            ("u", "t", "e", "c")
        );
        let blank = page_state(
            String::new(),
            String::new(),
            0,
            0,
            0,
            -5,
            String::new(),
            0,
            String::new(),
            0,
            -4,
        );
        assert_eq!(blank.progress, 0);
        assert!(!blank.secure);
        assert_eq!(blank.finished, 0, "a negative count is nothing");
        assert_eq!(state.finished, 3);
    }

    #[test]
    fn navigation_kinds_use_webkits_numbers_and_an_unknown_one_earns_no_trust() {
        assert_eq!(NavKind::from_code(0), NavKind::LinkClicked);
        assert_eq!(NavKind::from_code(1), NavKind::FormSubmitted);
        assert_eq!(NavKind::from_code(2), NavKind::BackForward);
        assert_eq!(NavKind::from_code(3), NavKind::Reload);
        assert_eq!(NavKind::from_code(4), NavKind::FormResubmitted);
        assert_eq!(NavKind::from_code(5), NavKind::Other);
        assert_eq!(NavKind::from_code(-1), NavKind::Other);
        assert_eq!(NavKind::from_code(900), NavKind::Other);
    }

    #[test]
    fn verdicts_have_the_documented_codes() {
        assert_eq!(Verdict::Allow.code(), 0);
        assert_eq!(Verdict::Deny.code(), 1);
        assert_eq!(Verdict::Handoff.code(), 2);
    }

    #[test]
    fn every_feature_is_its_own_bit_and_all_is_their_union() {
        use super::features::*;
        let each = [
            PAGE_UI,
            DIALOGS,
            DOWNLOADS,
            POLICY,
            OWN_MARKERS,
            PROFILE,
            NOTIFICATIONS,
        ];
        let mut seen = 0;
        for bit in each {
            assert_eq!(bit.count_ones(), 1);
            assert_eq!(seen & bit, 0, "{bit} is used twice");
            seen |= bit;
        }
        assert_eq!(ALL, seen);
    }

    #[test]
    fn a_notification_and_a_permission_request_become_the_windows_events() {
        assert_eq!(
            notify_show(
                7,
                "https://a.test".into(),
                "https://a.test/x".into(),
                "t".into(),
                "b".into(),
                "tag".into()
            ),
            Event::NotifyShow {
                id: 7,
                origin: "https://a.test".into(),
                page_url: "https://a.test/x".into(),
                title: "t".into(),
                body: "b".into(),
                tag: "tag".into(),
            }
        );
        let request = |gesture| Event::NotifyPermission {
            origin: "https://a.test".into(),
            user_gesture: gesture,
        };
        assert_eq!(notify_permission("https://a.test".into(), 1), request(true));
        assert_eq!(
            notify_permission("https://a.test".into(), 0),
            request(false)
        );
    }

    #[test]
    fn a_size_that_was_not_announced_is_none() {
        assert_eq!(byte_count(-1), None);
        assert_eq!(byte_count(0), Some(0));
        assert_eq!(byte_count(307_200), Some(307_200));
    }

    #[test]
    fn a_path_is_copied_with_its_nul_only_when_it_fits() {
        let mut out = [0xAAu8; 6];
        assert!(copy_c_string("abcde", &mut out) && &out == b"abcde\0");
        let mut out = [0xAAu8; 5];
        assert!(!copy_c_string("abcde", &mut out), "no room for the NUL");
        assert_eq!(out, [0xAA; 5], "left untouched");
        assert!(
            !copy_c_string("a\0b", &mut [0u8; 8]),
            "an embedded NUL is refused"
        );
        assert!(copy_c_string("", &mut [0xAAu8; 1]));
        assert!(!copy_c_string("", &mut []));
    }
}
