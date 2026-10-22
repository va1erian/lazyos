//! Which addresses a full view's page may load, and what becomes of the others (docs/webd-plan.md
//! 3.5 and section 5): pure functions over the view's [`Policy`], asked by the engine's thread for
//! every navigation (`webd_ui_navigate`, a decision on the spot) and by the service for a client's
//! own `Load`.
//!
//! * `about:`, `data:` and `blob:` load under every policy: they reach no network and read no file.
//! * `http:` and `https:` load under `Web`, and under `Origins` when the origin is listed; under
//!   `DataOnly` never (a view made without the network scope).
//! * `file:` never loads: webd's processes read no file of the person's (open question 5); the
//!   client reads and passes bytes with `LoadHtml`.
//! * Anything else (`mailto:`, `tel:`, an app's scheme) is a hand-off: refused, asked (`Ask`), or
//!   given to the program the person chose (`Browser`), and only for a navigation of the page
//!   itself that a user gesture started within [`INPUT_LOCK_MS`] of real input forwarded to the view:
//!   a page's script must never be able to start a program on its own.
//!
//! A navigation of a frame inside the page is never asked about or handed off: it is allowed or
//! refused, so a hidden frame cannot make the app or the person answer questions.

use crate::limits::INPUT_LOCK_MS;

/// `SchemePolicy` (idl/webd.midl).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schemes {
    Web,
    DataOnly,
    Origins,
}

/// `Handoff` (idl/webd.midl).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    Deny,
    Ask,
    Browser,
}

/// A view's navigation policy, fixed when it is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub schemes: Schemes,
    /// Normalised origins (`scheme://host[:port]`, lower case, the scheme's own port left out).
    pub origins: Vec<String>,
    pub handoff: Handoff,
}

/// `WEBD_VERDICT_*` (webd_abi.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow = 0,
    Deny = 1,
    HandOff = 2,
    Ask = 3,
}

/// What an address is, for the policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// about:, data:, blob:
    Local,
    /// http:, https:
    Network,
    /// file:, javascript:, an address with no scheme: never loaded, never handed off.
    Never,
    /// Any other scheme: a hand-off.
    Other,
}

/// The scheme of `url` in lower case, if it has a well-formed one.
pub fn scheme(url: &str) -> Option<String> {
    let end = url.find(':')?;
    let scheme = &url[..end];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

pub fn kind(url: &str) -> Kind {
    match scheme(url).as_deref() {
        Some("about" | "data" | "blob") => Kind::Local,
        Some("http" | "https") => Kind::Network,
        None | Some("file" | "javascript" | "vbscript") => Kind::Never,
        Some(_) => Kind::Other,
    }
}

/// The origin of an `http:` or `https:` address, normalised; `None` for anything else or a
/// malformed authority.
pub fn origin(url: &str) -> Option<String> {
    let scheme = scheme(url)?;
    let default_port = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return None,
    };
    // WebKit's URL parser reads `\` as `/` in an http(s) address (`https://evil.test\@example.com/` is
    // evil.test), and white space or a control character may be dropped or kept: an address with any
    // of them has no origin here, so it matches no allowlist entry.
    if url
        .chars()
        .any(|c| c == '\\' || c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let rest = url[scheme.len() + 1..].strip_prefix("//")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Credentials are not part of the origin.
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = match host_port.rfind(':') {
        // An IPv6 literal's colons are inside brackets.
        Some(at) if !host_port[at..].contains(']') => (&host_port[..at], &host_port[at + 1..]),
        _ => (host_port, ""),
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':'))
        || !port.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let host = host.to_ascii_lowercase();
    Some(if port.is_empty() || port == default_port {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    })
}

/// The host of an `http(s):` address (for the blocker's exceptions and a toast's title).
pub fn host(url: &str) -> Option<String> {
    let origin = origin(url)?;
    let after = &origin[origin.find("://")? + 3..];
    Some(match after.rfind(':') {
        Some(at) if !after[at..].contains(']') => after[..at].to_owned(),
        _ => after.to_owned(),
    })
}

impl Policy {
    /// Whether a network address may load under this policy.
    fn network_allowed(&self, url: &str) -> bool {
        match self.schemes {
            Schemes::Web => true,
            Schemes::DataOnly => false,
            Schemes::Origins => {
                origin(url).is_some_and(|origin| self.origins.iter().any(|o| *o == origin))
            }
        }
    }

    /// May the client itself load `url` in the view (`Load`, a `LoadHtml` base)? Only what the page
    /// could load: a client cannot widen its own view.
    pub fn client_may_load(&self, url: &str) -> bool {
        match kind(url) {
            Kind::Local => true,
            Kind::Network => self.network_allowed(url),
            Kind::Never | Kind::Other => false,
        }
    }

    /// The verdict for a navigation the engine is about to start. `recent_input`: real input reached
    /// the view within [`INPUT_LOCK_MS`].
    pub fn decide(
        &self,
        url: &str,
        gesture: bool,
        main_frame: bool,
        recent_input: bool,
    ) -> Verdict {
        match kind(url) {
            Kind::Local => Verdict::Allow,
            Kind::Network if self.network_allowed(url) => Verdict::Allow,
            Kind::Never => Verdict::Deny,
            // A network address the policy does not cover: only `Ask` can let it through, and only
            // for the page itself.
            Kind::Network => match self.handoff {
                Handoff::Ask if main_frame => Verdict::Ask,
                _ => Verdict::Deny,
            },
            Kind::Other if !main_frame => Verdict::Deny,
            Kind::Other => match self.handoff {
                Handoff::Deny => Verdict::Deny,
                Handoff::Ask => Verdict::Ask,
                Handoff::Browser if gesture && recent_input => Verdict::HandOff,
                Handoff::Browser => Verdict::Deny,
            },
        }
    }
}

/// Whether input at `last_input_ms` is recent at `now_ms` (0: never any input).
pub fn recent(last_input_ms: u64, now_ms: u64) -> bool {
    last_input_ms != 0 && now_ms.saturating_sub(last_input_ms) <= INPUT_LOCK_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(schemes: Schemes, handoff: Handoff) -> Policy {
        Policy {
            schemes,
            origins: vec!["http://10.0.2.2:18081".into(), "https://example.com".into()],
            handoff,
        }
    }

    #[test]
    fn origins_are_normalised() {
        assert_eq!(
            origin("HTTP://Example.COM:80/a?b#c").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(
            origin("https://u:p@example.com:8443/").as_deref(),
            Some("https://example.com:8443")
        );
        assert_eq!(
            origin("http://[::1]:81/").as_deref(),
            Some("http://[::1]:81")
        );
        assert_eq!(origin("http://[::1]/").as_deref(), Some("http://[::1]"));
        for bad in [
            "http:/x",
            "ftp://a",
            "http://",
            "http://a b/",
            "http://a:8x/",
            "data:x",
            "https://evil.test\\@example.com/",
            "https://evil.test:443\\@example.com/",
            "http://example.com\t@evil/",
        ] {
            assert_eq!(origin(bad), None, "{bad}");
        }
        assert_eq!(host("http://10.0.2.2:18081/x").as_deref(), Some("10.0.2.2"));
    }

    #[test]
    fn local_schemes_always_load_and_file_never_does() {
        for schemes in [Schemes::Web, Schemes::DataOnly, Schemes::Origins] {
            let p = policy(schemes, Handoff::Browser);
            for url in [
                "about:blank",
                "data:text/html,x",
                "blob:http://a/1",
                "ABOUT:blank",
            ] {
                assert_eq!(p.decide(url, false, true, false), Verdict::Allow, "{url}");
                assert!(p.client_may_load(url));
            }
            for url in [
                "file:///etc/passwd",
                "javascript:alert(1)",
                "nonsense",
                "1x:y",
            ] {
                assert_eq!(p.decide(url, true, true, true), Verdict::Deny, "{url}");
                assert!(!p.client_may_load(url));
            }
        }
    }

    #[test]
    fn the_network_follows_the_scheme_policy() {
        let web = policy(Schemes::Web, Handoff::Deny);
        let data = policy(Schemes::DataOnly, Handoff::Deny);
        let listed = policy(Schemes::Origins, Handoff::Deny);
        let url = "http://10.0.2.2:18081/page";
        assert_eq!(web.decide(url, false, false, false), Verdict::Allow);
        assert_eq!(data.decide(url, true, true, true), Verdict::Deny);
        assert!(!data.client_may_load(url));
        assert_eq!(listed.decide(url, false, true, false), Verdict::Allow);
        assert_eq!(
            listed.decide("http://10.0.2.2:18082/", true, true, true),
            Verdict::Deny
        );
        assert!(listed.client_may_load("https://example.com/x"));
        assert!(!listed.client_may_load("http://example.com/x"));
    }

    #[test]
    fn ask_holds_only_the_page_itself() {
        let p = policy(Schemes::Origins, Handoff::Ask);
        assert_eq!(
            p.decide("http://elsewhere/", false, true, false),
            Verdict::Ask
        );
        assert_eq!(
            p.decide("http://elsewhere/", false, false, false),
            Verdict::Deny
        );
        assert_eq!(p.decide("mailto:a@b", false, true, false), Verdict::Ask);
        assert_eq!(p.decide("mailto:a@b", true, false, true), Verdict::Deny);
    }

    #[test]
    fn a_hand_off_needs_a_gesture_and_recent_real_input() {
        let p = policy(Schemes::Web, Handoff::Browser);
        assert_eq!(p.decide("mailto:a@b", true, true, true), Verdict::HandOff);
        assert_eq!(p.decide("mailto:a@b", false, true, true), Verdict::Deny);
        assert_eq!(p.decide("mailto:a@b", true, true, false), Verdict::Deny);
        assert_eq!(p.decide("mailto:a@b", true, false, true), Verdict::Deny);
        assert_eq!(
            policy(Schemes::Web, Handoff::Deny).decide("mailto:a@b", true, true, true),
            Verdict::Deny
        );
        assert!(recent(1000, 1000 + INPUT_LOCK_MS));
        assert!(!recent(1000, 1001 + INPUT_LOCK_MS));
        assert!(!recent(0, 5));
    }
}
