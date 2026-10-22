//! Values the window and the engine agree on: how a navigation began, what became of a rule list,
//! a download's announced size, and which program argument is the first address. Pure and host-tested.

use crate::address;

/// The address to open first: the first argument after the program's name that is neither an
/// option (`-...`) nor the supervisor's `attempt=` note, made into a URL.
pub fn first_address(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1)
        .find(|arg| !arg.starts_with('-') && !arg.starts_with("attempt="))
        .and_then(|arg| address::normalize(arg))
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
    /// The wire value (`NavigationKind` of `idl/webd.midl`), WebKit's own numbering; anything unknown is `Other`, the kind that earns no
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

/// What became of a rule list the window gave the engine (webd's `BlockingList`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStatus {
    /// Stored and attached to the page.
    Ready,
    /// A lookup found nothing under that id.
    NotStored,
    /// Stored by another WebKit format version: compile it again.
    Stale,
    /// WebKit refused the JSON (one bad rule fails a whole list).
    CompileFailed,
    /// No store: no profile directory, or a WebKit built without content extensions.
    Unavailable,
}

impl ListStatus {
    /// The wire value (`BlockingStatus` of `idl/webd.midl`, the engine's `LZW_BLOCK_*`).
    pub const fn code(self) -> i32 {
        match self {
            ListStatus::Ready => 0,
            ListStatus::NotStored => 1,
            ListStatus::Stale => 2,
            ListStatus::CompileFailed => 3,
            ListStatus::Unavailable => 4,
        }
    }

    /// The status an ABI value names; anything unknown is a failure to compile, which the
    /// window answers by trying again from the source.
    pub const fn from_code(code: i32) -> ListStatus {
        match code {
            0 => ListStatus::Ready,
            1 => ListStatus::NotStored,
            2 => ListStatus::Stale,
            4 => ListStatus::Unavailable,
            _ => ListStatus::CompileFailed,
        }
    }

    /// The word markers and `about:blocker` use.
    pub const fn name(self) -> &'static str {
        match self {
            ListStatus::Ready => "ready",
            ListStatus::NotStored => "not-stored",
            ListStatus::Stale => "stale",
            ListStatus::CompileFailed => "compile-failed",
            ListStatus::Unavailable => "unavailable",
        }
    }
}

/// A download's size as the engine reports it: a negative number is "not announced".
pub fn byte_count(bytes: i64) -> Option<u64> {
    u64::try_from(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_first_address_skips_the_program_options_and_the_supervisors_note() {
        assert_eq!(first_address(&args(&["lazywebplus-app"])), None);
        assert_eq!(
            first_address(&args(&["lazywebplus-app", "example.com"])),
            Some("http://example.com/".to_owned())
        );
        assert_eq!(
            first_address(&args(&["lazywebplus-app", "--client", "attempt=1", "http://a/"])),
            Some("http://a/".to_owned())
        );
        assert_eq!(first_address(&args(&["lazywebplus-app", "attempt=2"])), None);
        // The program's own name is never an address.
        assert_eq!(first_address(&args(&["http://a/"])), None);
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
    fn list_statuses_round_trip_and_an_unknown_one_is_a_failure() {
        for status in [
            ListStatus::Ready,
            ListStatus::NotStored,
            ListStatus::Stale,
            ListStatus::CompileFailed,
            ListStatus::Unavailable,
        ] {
            assert_eq!(ListStatus::from_code(status.code()), status);
        }
        assert_eq!(ListStatus::from_code(77), ListStatus::CompileFailed);
        assert_eq!(ListStatus::from_code(-1), ListStatus::CompileFailed);
        assert_eq!(ListStatus::Stale.name(), "stale");
    }

    #[test]
    fn a_size_that_was_not_announced_is_none() {
        assert_eq!(byte_count(-1), None);
        assert_eq!(byte_count(0), Some(0));
        assert_eq!(byte_count(307_200), Some(307_200));
    }
}
