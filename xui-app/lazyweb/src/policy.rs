//! Which navigations the engine may make (`lzw_ui_navigate`, called from WebKit's
//! `decidePolicyForNavigationAction` for every navigation, redirect and window request).
//!
//! This is the rule a browser without scripts could take for granted, and WebKit
//! cannot: **a page's script must never be able to start another program or run one of the
//! browser's commands**. The engine tells the window whether a user gesture started the
//! navigation (`WKNavigationActionHasUnconsumedUserGesture`: a click, a key, not a timer, a
//! load event or a redirect) and the kind of navigation; the decision is a pure function of
//! those and of whether the page on screen is one of the browser's own:
//!
//! - the web, files and pages that carry their content (`http:`, `https:`, `file:`, `data:`,
//!   `blob:`, `about:`) load;
//! - `x-lazyweb:` commands run only from one of the browser's own pages (`about:history`, ...)
//!   and only for a click on a link there. They never go to another app, whoever asks;
//! - any other scheme (`mailto:`, `tel:`, ...) goes to the app registered for it, but only when a
//!   user gesture started it. Without one it is refused and reported (`LAUNCH:<url>:BLOCKED`).
//!
//! An address the user types never reaches this function for those schemes: the window opens
//! them itself (`handoff.rs`), which is why a navigation the UI process started (kind `Other`,
//! no gesture) to `mailto:` is a script's, a redirect's or a bug's, and is refused.

use xui_webkit::abi::NavKind;

use crate::address;
use crate::pages::Command;

/// The scheme of the browser's commands.
pub const COMMAND_SCHEME: &str = "x-lazyweb";

/// What to do with a navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The engine loads it.
    Allow,
    /// Give the URL to the app registered for its scheme.
    Handoff(String),
    /// Run a command of the browser's.
    Command(String),
    /// Refuse, and report it: something tried to start a program or a command without the user.
    Block(String),
}

/// Decides navigation `url` of kind `kind`. `user_gesture`: a click, key or typed address
/// started it. `from_own_page`: the page on screen is one of the browser's own (an exact one,
/// `Internal::name_for`), not a web page that looks like one.
pub fn decide(url: &str, kind: NavKind, user_gesture: bool, from_own_page: bool) -> Decision {
    let url = url.trim();
    if address::engine_loads(url) {
        return Decision::Allow;
    }
    if address::scheme(url).as_deref() == Some(COMMAND_SCHEME) {
        let a_click = kind == NavKind::LinkClicked;
        return if from_own_page && user_gesture && a_click && Command::parse(url).is_some() {
            Decision::Command(url.to_owned())
        } else {
            Decision::Block(url.to_owned())
        };
    }
    let started_by_the_user = user_gesture
        && matches!(
            kind,
            NavKind::LinkClicked | NavKind::FormSubmitted | NavKind::Other
        );
    if started_by_the_user {
        Decision::Handoff(url.to_owned())
    } else {
        Decision::Block(url.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAR: &str = "x-lazyweb:clear-history";

    #[test]
    fn the_web_loads_whoever_asks() {
        for url in [
            "http://example.com/",
            "https://example.com/a?b=c",
            "file:///docs/os/index.html",
            "data:text/html,hi",
            "about:blank",
        ] {
            for kind in [
                NavKind::LinkClicked,
                NavKind::Other,
                NavKind::BackForward,
                NavKind::Reload,
            ] {
                for gesture in [false, true] {
                    assert_eq!(decide(url, kind, gesture, false), Decision::Allow, "{url}");
                }
            }
        }
    }

    #[test]
    fn a_script_cannot_hand_a_link_to_another_app() {
        // `location = "mailto:..."` in a timer or a load handler: no gesture.
        for kind in [NavKind::Other, NavKind::LinkClicked, NavKind::FormSubmitted] {
            assert_eq!(
                decide("mailto:a@b.test", kind, false, false),
                Decision::Block("mailto:a@b.test".into()),
                "{kind:?}"
            );
        }
        // A server's redirect to a scheme is a navigation with no gesture either.
        assert_eq!(
            decide("tel:5551234", NavKind::Other, false, false),
            Decision::Block("tel:5551234".into())
        );
    }

    #[test]
    fn a_click_hands_a_link_to_the_app_registered_for_it() {
        assert_eq!(
            decide(
                "mailto:a@b.test?subject=hi",
                NavKind::LinkClicked,
                true,
                false
            ),
            Decision::Handoff("mailto:a@b.test?subject=hi".into())
        );
        // A button whose handler sets location rides the gesture of the click.
        assert_eq!(
            decide("MAILTO:a@b.test", NavKind::Other, true, false),
            Decision::Handoff("MAILTO:a@b.test".into())
        );
        // History and reload never start another program.
        for kind in [
            NavKind::BackForward,
            NavKind::Reload,
            NavKind::FormResubmitted,
        ] {
            assert_eq!(
                decide("mailto:a@b.test", kind, true, false),
                Decision::Block("mailto:a@b.test".into()),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn commands_run_only_from_the_browsers_own_pages_by_a_click() {
        assert_eq!(
            decide(CLEAR, NavKind::LinkClicked, true, true),
            Decision::Command(CLEAR.into())
        );
        assert_eq!(
            decide("x-lazyweb:open/3", NavKind::LinkClicked, true, true),
            Decision::Command("x-lazyweb:open/3".into())
        );
        // From a web page, with or without a gesture: nothing.
        for gesture in [false, true] {
            assert_eq!(
                decide(CLEAR, NavKind::LinkClicked, gesture, false),
                Decision::Block(CLEAR.into())
            );
        }
        // From our page but not by a person: a timer, a redirect.
        assert_eq!(
            decide(CLEAR, NavKind::LinkClicked, false, true),
            Decision::Block(CLEAR.into())
        );
        assert_eq!(
            decide(CLEAR, NavKind::Other, true, true),
            Decision::Block(CLEAR.into())
        );
        // A scheme the browser does not know, or a verb it does not have, is never a command.
        assert_eq!(
            decide("x-lazyweb:format-disk", NavKind::LinkClicked, true, true),
            Decision::Block("x-lazyweb:format-disk".into())
        );
    }

    #[test]
    fn a_command_scheme_is_never_handed_to_another_app() {
        for from_own_page in [false, true] {
            for gesture in [false, true] {
                for kind in [NavKind::LinkClicked, NavKind::Other, NavKind::FormSubmitted] {
                    let decision = decide("X-LazyWeb:clear-history", kind, gesture, from_own_page);
                    assert!(
                        !matches!(decision, Decision::Handoff(_)),
                        "{decision:?} for {kind:?} {gesture} {from_own_page}"
                    );
                }
            }
        }
    }

    #[test]
    fn whitespace_does_not_hide_a_scheme() {
        assert_eq!(
            decide("  mailto:a@b ", NavKind::LinkClicked, false, false),
            Decision::Block("mailto:a@b".into())
        );
        assert_eq!(
            decide(
                " x-lazyweb:clear-history",
                NavKind::LinkClicked,
                true,
                false
            ),
            Decision::Block(CLEAR.into())
        );
    }
}
