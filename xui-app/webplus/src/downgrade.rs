//! HTTPS first, and the way back to HTTP.
//!
//! A host typed in the address bar is opened over HTTPS. When the server does not answer there at
//! all (nothing listens, the connection is reset, it times out) the browser says so in a page of its
//! own and offers the same address over plain HTTP; the answer is remembered for the host until the
//! window closes. **A certificate or TLS error never offers it**: that is a server that does speak
//! TLS and fails the check, which an attacker who can strip HTTPS could just as well cause.
//!
//! webd tells a failed load by a sentence from the network stack (`Could not load <url>: <why>`),
//! so the kind of failure is read from that text; an error code from webd would be better and would
//! replace [`connection_failed`].

use std::collections::HashSet;

use crate::pages::{escape, page, percent_encode, SCHEME};

/// The page's name in the address bar.
pub const NAME: &str = "about:https";

/// The most hosts remembered: a person answers one question each, so this is never reached by use.
const MAX_ALLOWED: usize = 256;

/// An address typed as a host, opened over HTTPS and not yet known to have worked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    /// `host[:port]` in lower case.
    pub host: String,
    pub http: String,
}

/// The hosts the person allowed plain HTTP for in this window's life.
#[derive(Debug, Default)]
pub struct Allowed {
    hosts: HashSet<String>,
}

impl Allowed {
    pub fn contains(&self, host: &str) -> bool {
        self.hosts.contains(host)
    }

    pub fn insert(&mut self, host: &str) {
        if self.hosts.len() < MAX_ALLOWED {
            self.hosts.insert(host.to_owned());
        }
    }
}

/// Whether `error` (the engine's sentence for a failed load) says the connection to `host` over
/// HTTPS could not be made or kept, as opposed to a TLS or certificate failure, a DNS failure or an
/// HTTP answer.
pub fn connection_failed(error: &str, host: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    let own = format!("could not load https://{}", host.to_ascii_lowercase());
    // The host must end there: `example.com` is not `example.com.evil.test`.
    let ours = lower
        .strip_prefix(&own)
        .is_some_and(|rest| rest.starts_with([':', '/', '?', '#']));
    if !ours {
        return false;
    }
    // Anything that mentions the security layer is the server's answer, not its absence.
    const SECURITY: [&str; 5] = ["ssl", "tls", "certificate", "handshake", "secure"];
    if SECURITY.iter().any(|word| lower.contains(word)) {
        return false;
    }
    const NO_ANSWER: [&str; 8] = [
        "couldn't connect",
        "could not connect",
        "connection refused",
        "connection reset",
        "timeout",
        "timed out",
        "failure when receiving data",
        "server returned nothing",
    ];
    NO_ANSWER.iter().any(|phrase| lower.contains(phrase))
}

/// The command a click on the page's button runs: allow HTTP for `host`.
pub fn allow_command(host: &str) -> String {
    format!("{SCHEME}allow-http/{}", percent_encode(host))
}

/// The page that asks: the server at `host` did not answer over HTTPS.
pub fn question(host: &str) -> String {
    page(
        "This site did not answer over HTTPS",
        &format!(
            "<p>LazyWeb+ tried <b>{}</b> over an encrypted connection first, and nothing answered.</p>\
<p>Load it without encryption? Anything you send or receive can then be read or changed on the way.</p>\
<p><a class=\"button\" href=\"{}\">Load it without encryption</a></p>",
            escape(host),
            escape(&allow_command(host)),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_answer_offers_http() {
        let err = |why: &str| format!("Could not load https://example.com/: {why}");
        for why in [
            "Couldn't connect to server",
            "Connection refused",
            "Connection reset by peer",
            "Timeout was reached",
            "Operation timed out",
        ] {
            assert!(connection_failed(&err(why), "example.com"), "{why}");
        }
        for why in [
            "SSL peer certificate or SSH remote key was not OK",
            "Peer certificate cannot be authenticated with given CA certificates",
            "SSL connect error",
            "TLS handshake failed: connection reset",
            "Couldn't resolve host name",
            "HTTP error 404",
        ] {
            assert!(!connection_failed(&err(why), "example.com"), "{why}");
        }
    }

    #[test]
    fn the_failure_must_be_the_typed_hosts_over_https() {
        let refused = "Could not load https://example.com/: Connection refused";
        assert!(connection_failed(refused, "EXAMPLE.com"));
        assert!(!connection_failed(refused, "other.example"));
        assert!(!connection_failed(
            "Could not load http://example.com/: Connection refused",
            "example.com"
        ));
        assert!(!connection_failed(
            "The page's process stopped.",
            "example.com"
        ));
        assert!(connection_failed(
            "Could not load https://example.com:8443/x: Connection refused",
            "example.com:8443"
        ));
        // A host that only starts like the typed one is another host.
        assert!(!connection_failed(
            "Could not load https://example.com.evil.test/: Connection refused",
            "example.com"
        ));
    }

    #[test]
    fn the_page_names_the_host_escaped_and_carries_one_command() {
        let html = question("a<b>.example");
        assert!(html.contains("a&lt;b&gt;.example"));
        assert!(!html.contains("a<b>"));
        assert!(html.contains("x-lazyweb:allow-http/a%3Cb%3E.example"));
    }

    #[test]
    fn allowed_hosts_are_remembered_and_bounded() {
        let mut allowed = Allowed::default();
        assert!(!allowed.contains("a.example"));
        allowed.insert("a.example");
        assert!(allowed.contains("a.example"));
        for n in 0..1000 {
            allowed.insert(&format!("h{n}.example"));
        }
        assert!(allowed.hosts.len() <= MAX_ALLOWED);
    }
}
